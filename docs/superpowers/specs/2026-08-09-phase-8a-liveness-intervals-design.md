# Design: forge Phase 8a — Liveness, Intervals, ABI Foundations & Hints

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 1-5 + 15: RPO linearization, backward `live_in`/`live_out` liveness dataflow, `Interval` struct + construction, loop-back-edge extension (deferred — no loops exist yet), φ-interval handling, and register hints (two-address + φ operands). Lives in the new `crates/forge-regalloc` crate.
**Out of scope (deferred to later Phase 8 sub-slices)**: any actual register assignment (8b), spilling (8c), verification (8d), integration tests (8e).

## New dependency wiring

`crates/forge-regalloc/Cargo.toml` currently has an empty `[dependencies]` block. This slice adds:
```toml
[dependencies]
forge-ir = { path = "../forge-ir" }
forge-x64 = { path = "../forge-x64" }
```
`forge-ir` for `Value`, `Ty`, `Block`, `dominance::reverse_postorder`. `forge-x64` for `MachineInst`, `SelectedFunction`, `PhysReg`.

## Finding: `SelectedFunction` needs a new field before liveness can be computed at all

Liveness is a per-BLOCK backward dataflow (`live_out[B] = ⋃ live_in[S] for each successor S`; `live_in[B] = uses[B] ∪ (live_out[B] - defs[B])`), so it needs to know where each block's instructions start and end within `SelectedFunction::insts`, and which `Block` each `Jump`/`Branch` target refers to in terms of an actual position.

`SelectedFunction::insts: Vec<MachineInst>` is a single flat sequence with no block-boundary markers — `select()` (`crates/forge-x64/src/machine_inst/mod.rs:610-636`, the RPO walk itself at `:620-628`) walks `reverse_postorder(func)` and pushes each block's `MachineInst`s in turn, but never records where one block's instructions end and the next begin. Reconstructing this externally by re-counting IR instructions per block does NOT work: the mapping from IR `Inst` to `MachineInst` count is not 1:1 (`Fma` → 2 MachineInsts; `Phi` and lea-fusion-suppressed `Mul`/`Shl` → 0; everything else → 1), so only `select()` itself, which already performs this exact walk, can correctly record the boundaries.

**Resolution**: extend `SelectedFunction` with a new field, populated inside the existing `select()` walk:
```rust
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    pub coalescing_hints: HashMap<Value, Value>,
    pub pool: ConstantPool,
    /// (Block, first-instruction-index-in-insts) for every block, in the
    /// same RPO order `insts` itself was built in. Added in Phase 8a to
    /// let liveness analysis reconstruct block boundaries -- `insts` alone
    /// has no boundary markers, and the IR-instruction-to-MachineInst
    /// count isn't 1:1 (Fma emits 2, Phi/suppressed lea operands emit 0),
    /// so only `select()`'s own walk can record this correctly.
    pub block_starts: Vec<(Block, usize)>,
}
```
This is an additive, backward-compatible change to a type Phase 7 already grew incrementally three times (`coalescing_hints` in 7b, `pool` in 7c) — the same pattern, not a new one. `select()`'s body changes minimally: record `sel.insts.len()` immediately before each block's inner loop starts, push `(block, that_length)` to a new `Vec`. No existing field, test, or behavior changes. This touches `crates/forge-x64/src/machine_inst/mod.rs` (and its `tests.rs`, to add a `block_starts` assertion to at least one existing golden test) — a small, scoped amendment to already-shipped Phase 7 code, not a new file.

A block's end (for the LAST block in RPO) is `insts.len()`; for any other block, its end is the next entry's start in the `block_starts` list (since the list is in the same RPO order `insts` was built in, consecutive entries are adjacent ranges).

## Components

### `RegClass`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegClass {
    Gpr,
    Xmm,
}

impl RegClass {
    /// I64 and Bool both live in general-purpose registers (a Bool is a
    /// 0/1 GPR value per LoadImmI64's ConstBool handling, from Phase 7a);
    /// only F64 lives in XMM.
    pub fn of(ty: Ty) -> RegClass {
        match ty {
            Ty::I64 | Ty::Bool => RegClass::Gpr,
            Ty::F64 => RegClass::Xmm,
        }
    }
}
```

### ABI argument-register constants

```rust
/// System V AMD64 integer/pointer argument registers, in order.
pub const SYSV_INT_ARGS: &[PhysReg] =
    &[PhysReg::Rdi, PhysReg::Rsi, PhysReg::Rdx, PhysReg::Rcx, PhysReg::R8, PhysReg::R9];

/// System V AMD64 float argument registers, in order.
pub const SYSV_FLOAT_ARGS: &[PhysReg] =
    &[PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3,
      PhysReg::Xmm4, PhysReg::Xmm5, PhysReg::Xmm6, PhysReg::Xmm7];
```
These names don't collide with anything already shipped (unlike Phase 7d's `SYSV_CALLEE_SAVED`, which now has two same-named-but-different-membership constants in `SPEC.md` and `prologue.rs` — flagged and documented, not repeated here since `SYSV_INT_ARGS`/`SYSV_FLOAT_ARGS` are new names with no prior shipped constant to collide with).

**`Param`'s class-relative index**: `forge_ir::Inst::Param { index, .. }`'s `index` is the parameter's position across ALL parameters regardless of type (confirmed by reading `lower.rs:30-42`: `for (i, (name, ty)) in typed.params.iter().enumerate()`, one shared counter). But SysV assigns integer and float arguments from SEPARATE register files — the 3rd parameter overall could be the 1st float param and 2nd int param simultaneously, depending on the types of the params before it. `MachineInst::Param { dst, index }` only carries the overall index, so determining a `Param`'s fixed ABI register requires counting how many EARLIER params share its `RegClass`, not using `index` directly. Computed directly from `func.params: Vec<(String, Ty)>` (already public on `forge_ir::Function`, already 1:1 with `index`, and `build_intervals` already needs `func: &Function` for φ-handling) — `class_relative_index(index) = count of j in 0..index where RegClass::of(func.params[j].1) == RegClass::of(func.params[index].1)`. This reads `func.params` directly rather than re-deriving the same information by scanning `Param` MachineInsts in selection order — simpler, and doesn't depend on the (true, but non-obvious) fact that selection preserves declaration order.

### `Interval`

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct Interval {
    pub value: Value,
    pub start: u32,
    pub end: u32,
    pub reg_class: RegClass,
    pub hint: Option<Value>,
    pub fixed: Option<PhysReg>,
    pub spill_weight: f32,
}
```
`start`/`end` are positions into `SelectedFunction::insts` (the index IS the linear instruction number — bullet 1's "assign a sequential number to every instruction in RPO" is already satisfied for free by `Vec` indexing, since `select()` already builds `insts` by walking blocks in RPO; no separate numbering pass is needed). `start` is always the position where `value` is defined (every real SSA value has exactly one def). `end` is the position of `value`'s LAST use (or `start` itself, for a value that's computed but never used again before the function returns — e.g. directly returned with no other uses).

**`[start, end]` is INCLUSIVE, not the half-open `[start, end)` an earlier draft of this doc implied.** `end` is computed as the actual position of the last read, and that position is part of the live range (the value is still needed AT that instruction). Two intervals `[0, 2]` and `[2, 4]` DO overlap (both are live at position 2) — 8b's and 8d's overlap predicates must use `a.start <= b.end && b.start <= a.end`, not a half-open comparison. This was caught and corrected during plan-level execution review; stated here so 8b/8c/8d's own design docs inherit the correct convention rather than re-deriving or misreading it from the half-open phrasing that appeared earlier in this section.

**A φ's destination is a real interval even though it's never machine-defined**, and this creates two non-obvious liveness interactions 8b/8d must expect, not treat as bugs: (1) because a φ dst is genuinely READ (e.g. by a `Return`) but has no `MachineInst` that defines it, the backward dataflow makes it live-in all the way back to the function's entry block — harmless (the interval merge with its incoming values dominates the final range), but it means `live_in(entry)` is non-empty for any function containing a φ, which a naive verifier could mistake for "a value used before any definition." (2) Conversely, a φ's INCOMING value has no machine-level use of its own at all (nothing ever reads it directly — the φ merge is what keeps it alive) — liveness alone would give it a degenerate `[def, def]` single-point range; it's specifically the φ-interval merge (not liveness) that extends it to the join point. Both are correct, deliberate consequences of Phase 7a's "φ emits nothing" strategy, not gaps in the liveness algorithm.

### Liveness dataflow

Standard backward per-block iteration to a fixpoint, using `block_starts`/derived block ranges for block structure and `forge_ir::dominance::reverse_postorder`'s own ordering (processing blocks in REVERSE of RPO — i.e., postorder — converges fastest for backward dataflow, though correctness doesn't depend on visitation order, only on iterating until no `live_in`/`live_out` set changes).

Per-block `uses`/`defs` are computed once by scanning that block's `MachineInst` slice: a `MachineInst`'s operand `Value`s (`lhs`/`rhs`/`src`/`cond`/`args`/etc, per variant) are `uses` if not already defined earlier in the SAME block; its `dst` (where present) is a `def`. Block successors come directly from `MachineInst::Jump{target}`/`Branch{then_,else_}` (already carrying real `forge_ir::Block` ids) resolved through `block_starts` to find the successor's own use/def sets; `Return` has no successors.

### `Interval` construction from liveness

For each block, walk its instructions in order, tracking a live set seeded from `live_out[block]` and working backward: an instruction's `dst` value's interval starts there; each operand `Value`'s interval end is extended to at least this instruction's position (if not already extended further by a later use). A φ's destination additionally needs its interval SEEDED at its owning block's very first position, even though no `MachineInst` ever defines it (see the inclusive-range note above for why this matters) — without this seed, a φ dst would get no interval constructed for it at all, silently leaving it without a register despite genuinely being read later. Because `SelectedFunction::insts` is flat and cross-block, a value live across a block boundary (in `live_in`/`live_out` for the blocks it spans) has its interval's `end` extended to cover every position across ALL blocks it's live through, not just the block containing its actual last MachineInst-level use — this is exactly what makes an `Interval`'s `[start, end]` range meaningful for the SCAN algorithm (8b) even though the underlying representation is flat, not block-structured.

**Loop-back-edge extension (CHECKLIST bullet 4) is a no-op for now, documented as such, not silently skipped.** This project's IR currently has no loop construct (confirmed: `forge_ir::dominance` computes dominators/RPO over a DAG-shaped CFG; no back-edge-producing construct exists in the front-end grammar or `forge-syntax`). CHECKLIST's own bullet wording ("values live around a back-edge") already anticipates this as forward-looking, stretch-goal scope — same treatment as `Phi`'s critical-edge caveat from Phase 7a (a documented, unenforced invariant that holds today and must be re-verified if loops are ever added). This slice's liveness dataflow is written as a standard backward fixpoint iteration (which is loop-CORRECT by construction — fixpoint iteration handles back-edges automatically once the CFG actually has any, since it doesn't assume any particular visitation order converges in one pass), so no future rewrite is needed once loops exist; only the currently-true-by-construction absence of back-edges is left unenforced.

### φ-interval handling and the critical-edge obligation

CHECKLIST bullet 5: "an interval spans from the φ to all its incoming definitions." Since Phase 7a's `Phi` selection emits NO `MachineInst` at all (the φ's destination `Value` and every incoming `Value` are meant to end up sharing one physical location, resolved by "assign the same physical register/slot where possible; insert parallel-copy moves at predecessor block ends otherwise" — Phase 7a design doc's own words), this slice's job is to make that merge real at the `Interval` level: for each `Inst::Phi { incoming }` in the ORIGINAL IR (not present in `MachineInst` at all, so this requires reading `func: &Function` directly, not just `SelectedFunction` — confirming `build_intervals` needs BOTH `selected: &SelectedFunction` AND `func: &Function` as inputs), union the φ's own interval with each incoming value's interval into ONE `Interval` spanning the minimum `start` and maximum `end` across all of them.

**The merge must be order-independent and handle φ-chains/shared operands**, not just "process each φ once in `func.insts` order": a φ can feed another φ, or two φs can share an incoming value, and naive independent per-φ merging (in whatever order `func.insts` happens to list them) can produce inconsistent ranges depending on that order. The correct implementation is a union-find over every value a φ ties to another, collapsing each connected component into one shared `[min-start, max-end]` range in a single pass — not a sequence of independent pairwise unions.

**This slice MUST explicitly re-verify Phase 7a's critical-edge-free assumption before relying on it**, per the decomposition doc's own obligation. A critical edge (an edge from a block with multiple successors to a block with multiple predecessors) would make "one shared interval across dst+incoming" WRONG — if two different predecessors reach the φ's block via a critical edge each carrying a DIFFERENT value for the same φ, unioning them into a single interval would force both onto the same physical location even though they're genuinely different values needing different placement at different points. **Concrete verification plan**: add an assertion in `build_intervals` (or a dedicated pre-pass) that walks every `Inst::Phi` in `func`, and for each incoming `(pred_block, value)` pair, confirms `pred_block` has exactly one successor OR the φ's own block has exactly one predecessor total (counting ALL edges into it, not just this one) — the standard critical-edge definition — `assert!`, not `debug_assert!`, matching this project's "caller/data bugs must fail loudly in release too" precedent (Phase 6a's `bind()`, Phase 7d's `Rbp`-in-`callee_saved` guard). Given today's front-end only produces if/else DAGs (confirmed: no loop, no unstructured-goto construct), this assertion should never fire on any currently-producible input — it exists purely as a tripwire for whenever the front-end grows a construct that COULD introduce one.

**Both counts must come from real terminators, not from `BlockData::preds`.** `forge_ir::ir::BlockData` does carry a `preds` field, populated by `Builder::add_pred` during SSA construction — but that's `Builder`'s own bookkeeping, which a caller can forget to update, leave stale, or never populate at all for a hand-built `Function` (this slice's own tests construct `Function`s by hand without always calling `add_pred` correctly). A block's terminator (`Jump`/`Branch`/`Return`) is the actual ground truth of the CFG the selector laid out and walked — deriving both the "how many successors does `pred_block` have" and "how many total predecessors does the φ's block have" counts by scanning terminators (the latter requires one pass building a target→count map across ALL blocks, not just this φ's own incoming list) is more robust than trusting `preds` and cannot disagree with what `select()` itself actually processed.

### Hints (CHECKLIST bullet 15)

Two sources, both populated into `Interval::hint` at construction time (not left for 8b to compute):
1. **Two-address hints**: read directly from `SelectedFunction::coalescing_hints: HashMap<Value, Value>` (fully computed already, Phase 7b) — for each entry `dst -> preferred_same_as`, if `preferred_same_as` already has a known/hintable location (its own hint, or eventually its assigned register once 8b runs — but at INTERVAL-CONSTRUCTION time, before any assignment exists, the most that can be recorded is "dst should share `preferred_same_as`'s eventual interval," which in practice means: don't try to resolve a concrete `PhysReg` here at all — hint should point at the copy relationship, and 8b's `pick_register` is what actually looks up "what did `preferred_same_as` get assigned" at scan time (since scanning `active`-order processes earlier-starting intervals first, and `dst`'s hinted source is always an earlier-defined value, so it's always already been assigned SOME location by the time `dst`'s own turn comes, and 8b can then read that assignment). **This means `Interval::hint` should NOT be `Option<PhysReg>` as SPEC.md's pseudocode states — it should be `Option<Value>`** (the value to try to co-locate with), resolved to an actual `PhysReg` at 8b's scan time, not at 8a's construction time. This is a real, deliberate deviation from SPEC.md's literal struct shape, flagged explicitly rather than silently diverging — SPEC.md's pseudocode is this project's established "prose sketch, not literal contract" source (already diverged from productively in multiple Phase 7 designs, e.g. `ret()`'s stdcall/cdecl comment fix), and `Option<PhysReg>` genuinely cannot represent "hint towards whatever register some OTHER not-yet-allocated value ends up in."
2. **φ-operand hints**: **CORRECTED — the merge alone is NOT sufficient, and an earlier revision of this doc was wrong to claim it was.** A merged φ-group is represented as N separate `Interval` entries (one per original `Value`: the φ's own destination plus every incoming value) sharing an identical `[start, end]` range — NOT collapsed into a literal single `Interval`, because `Interval::value` is a single `Value` and there is no group-identity type in this struct. To a linear-scan allocator that has no other signal, N intervals with identical, fully-overlapping ranges look like N MUTUALLY INTERFERING values needing N DIFFERENT registers — the exact opposite of what a φ-group needs (ALL members must end up sharing ONE physical location, or Phase 7a's "φ emits nothing" strategy is unsound, since nothing else ever copies between them). This was caught during Task 4/5's post-implementation review by constructing `if a > b then a else b` and observing the allocator-facing data was contradictory by construction. **Fix**: every member of a φ-group ALSO gets a mutual `hint` toward one canonical anchor — reusing the existing `hint: Option<Value>` mechanism, not a new field. This gives 8b's ordinary hint-preference machinery a real chance to naturally co-locate the group. It is NOT a guarantee (hints are soft, per this project's established "a hint that isn't honored is not an error" convention) — when register pressure or a spill forces a group member into a different location than the rest, the group is left correctly overlapping-but-hinted, and it becomes the deferred final-emission task's job to detect any φ-group whose members didn't end up co-located and insert a REAL parallel copy at the relevant predecessor block's end, which is exactly the fallback Phase 7a's own design doc already anticipated ("insert parallel-copy moves at predecessor block ends otherwise") but which doesn't exist yet.

**CORRECTED TWICE — the canonical anchor is NOT the φ's own destination, and this ordering detail is load-bearing, not cosmetic.** An earlier revision of this fix anchored every group member's hint at the φ's own `Value`. This is wrong: after the range merge, ALL group members share an IDENTICAL `[start, end]` — so when 8b sorts intervals by `(start, end, value)` for scan order (the deterministic tie-break this doc's own "deterministic return order" fix established), ties break on raw `Value` number, and a φ's own `Value` is created LATER than its incoming values in `func.insts` (SSA construction: predecessor blocks' instructions, including the values a φ merges, are always built before the join block seals and mints the φ) — meaning the φ is almost always the HIGHEST `Value` number, hence almost always the LAST group member 8b's scan reaches, not the first. Hinting everyone TOWARD the φ therefore points every hint FORWARD (toward a not-yet-assigned interval), directly violating this section's own stated contract below ("a hint always points at an earlier-starting interval") — confirmed empirically: on `if a > b then a + b else a - b`, both `a+b` and `a-b`'s two-address hints AND the φ mutual-hints all pointed at intervals 8b hadn't assigned yet when it needed them.

**Correct rule: the anchor is the group's SMALLEST `Value` number, not the φ's own `Value`.** Every OTHER member (whichever Values those are, φ included) hints toward that minimum. Since sort ties break on `Value` number ascending, the smallest-numbered member is always processed FIRST among the tied group, so every other member's hint genuinely points backward, satisfying the "hinted value is always already assigned" contract for real. This changes nothing about WHAT gets merged (still the whole φ-group, computed identically) — only WHICH member is treated as the "already decided" anchor the rest defer to.

**Also corrected — precedence against two-address hints.** `merge_phi_intervals` runs before `populate_two_address_hints` in `build_intervals`'s pipeline; the two-address pass must NOT unconditionally overwrite a hint the φ pass already set (an earlier revision did exactly this, silently discarding the hard φ-coalescing signal in favor of the strictly-softer, always-emission-fixable two-address signal, on the very simplest branching program with two-address arithmetic on both arms). `populate_two_address_hints` must check `iv.hint.is_none()` before writing — φ-derived hints always win over two-address-derived hints, since φ-coalescing has no cheap emission-time fallback yet (real parallel-copy insertion, not built) while two-address mismatches are always trivially fixable by one `mov`.

**Note for 8b's own design doc**: resolving `Interval::hint: Option<Value>` into an actual register is 8b's job, done by looking up the hinted `Value` in 8b's own scan-time `assignment: FxHashMap<Value, Location>` map (already part of SPEC.md's `LinearScan` struct shape) — since intervals are processed in `start`-order (with `(start, end, value)` as the full deterministic sort key, per the "deterministic return order" fix) and a hint always points at a SMALLER-`(start,end,value)`-ordered interval by construction (both for ordinary two-address hints and for the corrected φ-group anchor rule above), the hinted value is always already assigned by the time it's looked up. This is a real lookup step 8b's design doc must state explicitly, not something it gets by "just reading a field" the way `Option<PhysReg>` would have allowed.

### Fixed registers — CORRECTED after Task 4/5's post-implementation review found the original design unsatisfiable-by-construction on trivial programs

**The problem an earlier revision of this doc missed**: treating `Interval::fixed: Option<PhysReg>` as a WHOLE-LIFETIME pin (the value must occupy exactly this register for its entire `[start, end]` range) is what CHECKLIST bullet 10 and PROMPT.md's `evict_and_assign` sketch literally describe, but it produces genuinely UNSATISFIABLE constraint sets on ordinary programs. Two concrete, reachable examples, both confirmed by actually running the shipped code:
- `((a >> 1) % (b >> 1)) + (c >> 1)` (three int params, one `%`): param `c` is the 3rd int arg, `SYSV_INT_ARGS[2] == Rdx` — its interval is `fixed = Some(Rdx)` for its WHOLE range (it's used again in the final `+`). `IntRem`'s `dst` is ALSO `fixed = Some(Rdx)` for its whole range. Their ranges genuinely overlap (both survive to the final `Add`). No assignment can satisfy "both, forever, in the same register but distinct values."
- Even without any `Param` involved: `a/b + c/d` computes two independent divisions whose quotients are both needed by the final `Add` — BOTH `IntDiv`'s `dst`s get `fixed = Some(Rax)` for their whole (overlapping) ranges. Same unsatisfiable clash, and MORE commonly reachable than the `Param` case, since it requires nothing but two divisions used together.

**Root cause, once traced through**: none of `Param`'s ABI register, `IntDiv`'s `Rax`, or `IntRem`'s `Rdx` are actually whole-lifetime requirements — each is a requirement that holds for exactly ONE instruction (the `Param`'s own position, for a value that's about to be READ out of that register at function entry; the `IntDiv`/`IntRem`'s own position, for a value that's about to be WRITTEN into that register by `idiv`). After that single instant, the value is an entirely ordinary virtual register with no remaining hardware constraint. `Interval::fixed` as specified can only express "always," not "at this one instruction" — real interval SPLITTING (breaking one value's lifetime into independently-assignable pieces joined by an inserted copy) is the textbook fix, but this project's linear-scan design (PROMPT.md's own sketch) has no splitting mechanism at all, and adding one is far more machinery than these three call sites justify.

**Resolution: `Param`/`IntDiv`/`IntRem`'s `dst` DO NOT populate `Interval::fixed` at all.** Instead, each is treated as a STATICALLY-KNOWN, always-re-derivable-from-the-MachineInst-itself emission-time fixup — exactly the same category as two-address fixups and the `lhs`-into-`Rax` dividend copy this doc already resolved this way:
- `Param { dst, index }`: no interval-level marking of any kind. The deferred final-emission task, when it reaches a `Param` MachineInst, independently recomputes which ABI register held the raw incoming value (a pure function of `func.params` and `index` — no `Interval` data needed) and inserts `mov <dst's real assigned location>, <ABI register>` UNLESS they already coincide. This mirrors the two-address-hint fallback's own documented rule ("a hint that isn't honored is not an error — emission falls back to inserting the copy").
- `IntDiv { dst, .. }` / `IntRem { dst, .. }`: no interval-level marking on `dst` either. Emission, right after emitting `idiv`, inserts `mov <dst's real assigned location>, Rax` (or `Rdx` for `IntRem`) unless they coincide — symmetric to the existing `lhs`-into-`Rax` fixup this doc already specifies for the dividend side.
- `rhs` (the divisor) for BOTH `IntDiv` and `IntRem`: UNCHANGED from the prior resolution — still excluded from `Rax`/`Rdx` in 8b's candidate register set (see below). This one genuinely CANNOT be fixed by a later copy, since `cqo`/`idiv` destroys the divisor's value irrecoverably before any copy could run, so it remains a real allocation-time constraint, not an emission-time one.
- `lhs` (the dividend): UNCHANGED — still gets no interval-level treatment, resolved by the emission-time copy this doc already specified.

**What `Interval::fixed` is for, then, going forward**: the field stays in the struct (CHECKLIST bullet 10 and PROMPT.md's `evict_and_assign` establish it as a real, intended concept, and a genuinely whole-lifetime hardware constraint may exist in some future `MachineInst` variant), but as of Phase 8a, NO current rule populates it — every currently-known "fixed register" requirement turned out to be a point constraint resolvable via emission-time copy instead. `fixed`'s doc comment should say this plainly so a future contributor doesn't wonder why it's always `None`.

**The ABI-overflow check is UNCHANGED and still real**: `class_relative_index` exceeding the ABI register list's length (>6 int or >8 float params) still gets an `assert!`, not silent mishandling, for the same reasons as before (this front-end has no explicit parameter-declaration syntax — params are inferred from free identifiers, so `a+b+c+d+e+f+g` trivially has 7 int params with no adversarial effort — and this MUST become a real `Diagnostic` before any user-facing CLI ships, Phase 13, tracked as a known follow-up). This check doesn't depend on whether `fixed` gets populated — it's validating that the ABI has room for the parameter at all, independent of how that fact later gets communicated to emission.

**The idiv "other register" clobber problem — unchanged in substance, restated for the corrected model**: `idiv` clobbers BOTH `rax` and `rdx` regardless of which one is semantically "the result" (CHECKLIST.md:275's "force eviction of whoever holds **them**" — plural). Three sub-problems:
1. **An unrelated third-party value happens to be live in the OTHER register at this program point.** Deferred to the final-emission step, AS LONG AS 8c/8d retain full `Interval` + final `Value -> Location` assignment data past 8b — an explicit exit criterion for 8c/8d.
2. **`rhs` assigned to `rax`/`rdx` by an unconstrained `pick_register`.** Not fixable by a later copy (the divisor's value is destroyed before `idiv` ever reads it) — fixed by the candidate-set exclusion (Task 6's `excluded_registers`), unchanged.
3. **`lhs`/`dst` needing specific registers around the instruction.** Now UNIFORMLY resolved by emission-time copies for BOTH sides (dividend-in, result-out), not just the dividend side as an earlier revision of this doc had it — see above.

This keeps `compute_coalescing_hints`'s own existing promise (`machine_inst/mod.rs`, ~line 641: "IntDiv/IntRem are deliberately excluded [from generic 2-address hints] — their constraint is fixed RAX/RDX placement, a different (fixed-register, not coalescing) hint Phase 8's allocator handles separately") — "handles separately" now concretely means: `rhs` gets a candidate-set exclusion (the one thing that's genuinely allocation-time), `lhs` and `dst` get nothing at the interval level at all (both resolved by emission-time copies, derivable with zero `Interval` involvement).

## Testing

- `RegClass::of` for all 3 `Ty` variants.
- `SYSV_INT_ARGS`/`SYSV_FLOAT_ARGS` contents match SPEC.md's documented lists.
- `block_starts` on a multi-block function (if/else) — confirms real block boundaries recovered, tested via a new assertion added to an existing `machine_inst/tests.rs` golden test plus at least one new test with a real branch.
- `build_intervals` on a single straight-line function (no branches) — golden `Vec<Interval>` test, confirming `start`/`end` positions match hand-traced expectations.
- `build_intervals` on an if/else function with a value live across the branch — confirms cross-block interval extension works (the value's `end` extends into the join block, past where it's textually last used in its OWN block).
- A φ-merge test: confirms a φ's interval and its incoming values' intervals get the correct merged min-start/max-end range, AND that every group member EXCEPT the smallest-`Value`-numbered one hints toward that smallest member — the merge alone is not sufficient (see the corrected Hints section), and the anchor is NOT the φ's own destination (see the twice-corrected reasoning above; a φ-as-anchor test would look plausible but silently fail 8b's backward-hint-resolution contract). A test using TWO-ADDRESS-hinted incoming values (e.g. `a + b` / `a - b` as the if/else arms, not bare constants) is required to actually exercise the precedence fix — a constants-only fixture cannot distinguish "φ hint set" from "φ hint correctly WON over a competing two-address hint," since there's no competing hint to lose to.
- The critical-edge assertion: confirms it does NOT fire on any of this project's currently-producible if/else programs (a negative test — build a handful of real if/else `Function`s via the front-end/builder and confirm `build_intervals` never panics), AND confirms it DOES fire on a hand-built critical edge (a shape the front-end can't produce today, but the tripwire must genuinely detect, not just appear to).
- Hint tests: a two-address-destructive op's `dst` gets `hint: Some(lhs_value)`; an op with no natural hint gets `hint: None`.
- `Param`'s class-relative-index computation is still tested (the ABI register a param WOULD occupy, per the corrected resolution recomputed independently by the deferred final-emission task, not stored on `Interval` at all) — a function with mixed I64/F64/Bool params in a specific order exercises `class_relative_index`'s correctness (e.g. `f(x: f64, n: i64, y: f64)` → `x`'s class-relative index is 0 within XMM, `n`'s is 0 within GPR, `y`'s is 1 within XMM — NOT the raw `index`-based values). Confirms NO `Interval` in the returned set has `fixed: Some(_)` as a side effect of this rule (the corrected behavior).
- A `Param`-count-overflow test: a function with 7 int params `#[should_panic]`s — UNCHANGED, this check doesn't depend on whether `fixed` gets populated.
- `IntDiv`/`IntRem` tests: CORRECTED — `dst`'s interval gets `fixed: None` (not `Some(Rax)`/`Some(Rdx)` as an earlier revision specified), confirming the "no interval-level marking, pure emission-time fixup" resolution now applies to `dst` as well as `lhs`. A separate test confirms `rhs`'s exclusion-from-`Rax`/`Rdx` side channel is unaffected by this change.
- A `rhs`-candidate-exclusion test (8b-level, but the underlying fact — WHICH registers are excluded for which `Value` at which instruction — must be exposed by 8a's output in some form for 8b to consume; this slice's design doc leaves the exact exposure mechanism, e.g. a side-channel map from `(instruction position, Value) -> excluded PhysRegs`, to the implementation plan, and flags it as a real open item 8b's design doc must confirm it received).

## Exit criteria

1. `crates/forge-regalloc/Cargo.toml` depends on `forge-ir` and `forge-x64`.
2. `SelectedFunction` gains a `block_starts: Vec<(Block, usize)>` field, populated correctly by `select()`, with no change to any other field or existing test's expected values.
3. `RegClass`, `SYSV_INT_ARGS`, `SYSV_FLOAT_ARGS` exist and are correct.
4. `Interval` struct exists, with `hint: Option<Value>` (a deliberate, documented deviation from SPEC.md's `Option<PhysReg>`).
5. `build_intervals(func: &Function, selected: &SelectedFunction) -> Vec<Interval>` exists, correctly computing `start`/`end` from real backward liveness dataflow (not an approximation), correctly merging φ intervals (range AND mutual hints — the merge alone is insufficient, per the corrected Hints section), and correctly populating two-address hints. `Param`/`IntDiv`/`IntRem` do NOT populate `Interval::fixed` at all (corrected — the original whole-lifetime-pin design was unsatisfiable-by-construction; see the corrected "Fixed registers" section).
6. The critical-edge tripwire assertion exists, never fires on any currently-producible program, and DOES fire on a hand-built critical edge (both directions tested, not just the negative case).
7. The idiv clobber problem is resolved per this doc's corrected three-part split: `rhs` candidate-excluded (exposed for 8b to consume, exact shape left to the plan — the one genuinely allocation-time constraint), `lhs` AND `dst` both left untouched at the interval level (pure emission-time fixup for both sides, no allocator-level change for either).
8. `Param`s exceeding the ABI argument-register count `assert!` (not silently mishandled), with an explicit note that this must become a real `Diagnostic` before any user-facing CLI surface ships (Phase 13) — tracked, not closed. Unaffected by the `fixed`-population correction.
9. `Vec<Interval>`'s return order is deterministic (sorted, e.g. by `(start, end, value)`) — a `HashMap`-backed construction without an explicit final sort is NOT acceptable, since nondeterministic register assignment across otherwise-identical runs would make 8b's own golden tests flaky and machine code non-reproducible. Caught during Task 4/5's post-implementation review; not part of any earlier revision of this doc.
10. `successors_of`'s treatment of a degenerate `Branch { then_: X, else_: X }` (both arms targeting the same block) does not cause the critical-edge tripwire to misfire on a non-critical edge — not reachable from today's front-end, but flagged during Task 4/5's review as a real gap worth guarding before Phase 7f (which could plausibly synthesize such a branch) lands. Either dedupe successors before counting, or count distinct source blocks rather than edges when computing `phi_block_predecessors`.
11. Tests cover every item in "Testing" above.
12. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
13. No regressions in any existing test, including `machine_inst/tests.rs`'s golden tests (which must still pass with `block_starts` added).
14. `Interval`/final assignment data (not just a flattened `Value -> PhysReg` table) is retained in whatever `SelectedFunction`/8b/8c output shape carries forward to the deferred final-emission task — required for that task to handle idiv's third-party-clobber case, the NOW-THREE emission-time fixups (`Param`'s entry copy, `IntDiv`/`IntRem`'s dividend-in and result-out copies), and any other program-point-specific save/restore need.
15. Any φ-group whose members do NOT end up co-located after 8b/8c run is detectable by the deferred final-emission task (by re-walking `Inst::Phi` in `func`, the same way `merge_phi_intervals` does) and gets a real parallel copy inserted at the relevant predecessor block's end — the fallback Phase 7a's own design doc already anticipated but which doesn't exist yet. Not this slice's job to BUILD, but its job to leave the data in a state where it's buildable.
