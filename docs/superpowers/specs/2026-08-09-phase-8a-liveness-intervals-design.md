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

`SelectedFunction::insts: Vec<MachineInst>` is a single flat sequence with no block-boundary markers — `select()` (`crates/forge-x64/src/machine_inst/mod.rs:610-618`) walks `reverse_postorder(func)` and pushes each block's `MachineInst`s in turn, but never records where one block's instructions end and the next begin. Reconstructing this externally by re-counting IR instructions per block does NOT work: the mapping from IR `Inst` to `MachineInst` count is not 1:1 (`Fma` → 2 MachineInsts; `Phi` and lea-fusion-suppressed `Mul`/`Shl` → 0; everything else → 1), so only `select()` itself, which already performs this exact walk, can correctly record the boundaries.

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

**`Param`'s class-relative index**: `forge_ir::Inst::Param { index, .. }`'s `index` is the parameter's position across ALL parameters regardless of type (confirmed by reading `lower.rs:30-42`: `for (i, (name, ty)) in typed.params.iter().enumerate()`, one shared counter). But SysV assigns integer and float arguments from SEPARATE register files — the 3rd parameter overall could be the 1st float param and 2nd int param simultaneously, depending on the types of the params before it. `MachineInst::Param { dst, index }` only carries the overall index, so determining a `Param`'s fixed ABI register requires counting how many EARLIER params share its `RegClass`, not using `index` directly. This must be computed once per function (a simple linear scan over `Param` MachineInsts in order, incrementing a per-class counter) — a real, non-obvious step this slice's interval-construction code must perform, not a simple table lookup.

### `Interval`

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct Interval {
    pub value: Value,
    pub start: u32,
    pub end: u32,
    pub reg_class: RegClass,
    pub hint: Option<PhysReg>,
    pub fixed: Option<PhysReg>,
    pub spill_weight: f32,
}
```
`start`/`end` are positions into `SelectedFunction::insts` (the index IS the linear instruction number — bullet 1's "assign a sequential number to every instruction in RPO" is already satisfied for free by `Vec` indexing, since `select()` already builds `insts` by walking blocks in RPO; no separate numbering pass is needed). `start` is always the position where `value` is defined (every real SSA value has exactly one def). `end` is the position of `value`'s LAST use (or `start` itself, for a value that's computed but never used again before the function returns — e.g. directly returned with no other uses).

### Liveness dataflow

Standard backward per-block iteration to a fixpoint, using `block_starts`/derived block ranges for block structure and `forge_ir::dominance::reverse_postorder`'s own ordering (processing blocks in REVERSE of RPO — i.e., postorder — converges fastest for backward dataflow, though correctness doesn't depend on visitation order, only on iterating until no `live_in`/`live_out` set changes).

Per-block `uses`/`defs` are computed once by scanning that block's `MachineInst` slice: a `MachineInst`'s operand `Value`s (`lhs`/`rhs`/`src`/`cond`/`args`/etc, per variant) are `uses` if not already defined earlier in the SAME block; its `dst` (where present) is a `def`. Block successors come directly from `MachineInst::Jump{target}`/`Branch{then_,else_}` (already carrying real `forge_ir::Block` ids) resolved through `block_starts` to find the successor's own use/def sets; `Return` has no successors.

### `Interval` construction from liveness

For each block, walk its instructions in order, tracking a live set seeded from `live_out[block]` and working backward: an instruction's `dst` value's interval starts there; each operand `Value`'s interval end is extended to at least this instruction's position (if not already extended further by a later use). Because `SelectedFunction::insts` is flat and cross-block, a value live across a block boundary (in `live_in`/`live_out` for the blocks it spans) has its interval's `end` extended to cover every position across ALL blocks it's live through, not just the block containing its actual last MachineInst-level use — this is exactly what makes an `Interval`'s `[start, end)` range meaningful for the SCAN algorithm (8b) even though the underlying representation is flat, not block-structured.

**Loop-back-edge extension (CHECKLIST bullet 4) is a no-op for now, documented as such, not silently skipped.** This project's IR currently has no loop construct (confirmed: `forge_ir::dominance` computes dominators/RPO over a DAG-shaped CFG; no back-edge-producing construct exists in the front-end grammar or `forge-syntax`). CHECKLIST's own bullet wording ("values live around a back-edge") already anticipates this as forward-looking, stretch-goal scope — same treatment as `Phi`'s critical-edge caveat from Phase 7a (a documented, unenforced invariant that holds today and must be re-verified if loops are ever added). This slice's liveness dataflow is written as a standard backward fixpoint iteration (which is loop-CORRECT by construction — fixpoint iteration handles back-edges automatically once the CFG actually has any, since it doesn't assume any particular visitation order converges in one pass), so no future rewrite is needed once loops exist; only the currently-true-by-construction absence of back-edges is left unenforced.

### φ-interval handling and the critical-edge obligation

CHECKLIST bullet 5: "an interval spans from the φ to all its incoming definitions." Since Phase 7a's `Phi` selection emits NO `MachineInst` at all (the φ's destination `Value` and every incoming `Value` are meant to end up sharing one physical location, resolved by "assign the same physical register/slot where possible; insert parallel-copy moves at predecessor block ends otherwise" — Phase 7a design doc's own words), this slice's job is to make that merge real at the `Interval` level: for each `Inst::Phi { incoming }` in the ORIGINAL IR (not present in `MachineInst` at all, so this requires reading `func: &Function` directly, not just `SelectedFunction` — confirming `build_intervals` needs BOTH `selected: &SelectedFunction` AND `func: &Function` as inputs), union the φ's own interval with each incoming value's interval into ONE `Interval` spanning the minimum `start` and maximum `end` across all of them.

**This slice MUST explicitly re-verify Phase 7a's critical-edge-free assumption before relying on it**, per the decomposition doc's own obligation. A critical edge (an edge from a block with multiple successors to a block with multiple predecessors) would make "one shared interval across dst+incoming" WRONG — if two different predecessors reach the φ's block via a critical edge each carrying a DIFFERENT value for the same φ, unioning them into a single interval would force both onto the same physical location even though they're genuinely different values needing different placement at different points. **Concrete verification plan**: add an assertion in `build_intervals` (or a dedicated pre-pass) that walks every `Inst::Phi` in `func`, and for each incoming `(pred_block, value)` pair, confirms `pred_block` has exactly one successor OR the φ's own block has exactly one predecessor from that edge (the standard critical-edge definition) — `assert!`, not `debug_assert!`, matching this project's "caller/data bugs must fail loudly in release too" precedent (Phase 6a's `bind()`, Phase 7d's `Rbp`-in-`callee_saved` guard). Given today's front-end only produces if/else DAGs (confirmed: no loop, no unstructured-goto construct), this assertion should never fire on any currently-producible input — it exists purely as a tripwire for whenever the front-end grows a construct that COULD introduce one.

### Hints (CHECKLIST bullet 15)

Two sources, both populated into `Interval::hint` at construction time (not left for 8b to compute):
1. **Two-address hints**: read directly from `SelectedFunction::coalescing_hints: HashMap<Value, Value>` (fully computed already, Phase 7b) — for each entry `dst -> preferred_same_as`, if `preferred_same_as` already has a known/hintable location (its own hint, or eventually its assigned register once 8b runs — but at INTERVAL-CONSTRUCTION time, before any assignment exists, the most that can be recorded is "dst should share `preferred_same_as`'s eventual interval," which in practice means: don't try to resolve a concrete `PhysReg` here at all — hint should point at the copy relationship, and 8b's `pick_register` is what actually looks up "what did `preferred_same_as` get assigned" at scan time (since scanning `active`-order processes earlier-starting intervals first, and `dst`'s hinted source is always an earlier-defined value, so it's always already been assigned SOME location by the time `dst`'s own turn comes, and 8b can then read that assignment). **This means `Interval::hint` should NOT be `Option<PhysReg>` as SPEC.md's pseudocode states — it should be `Option<Value>`** (the value to try to co-locate with), resolved to an actual `PhysReg` at 8b's scan time, not at 8a's construction time. This is a real, deliberate deviation from SPEC.md's literal struct shape, flagged explicitly rather than silently diverging — SPEC.md's pseudocode is this project's established "prose sketch, not literal contract" source (already diverged from productively in multiple Phase 7 designs, e.g. `ret()`'s stdcall/cdecl comment fix), and `Option<PhysReg>` genuinely cannot represent "hint towards whatever register some OTHER not-yet-allocated value ends up in."
2. **φ-operand hints**: for a merged φ-interval (see above), no additional hint is needed beyond the merge itself — since the φ and all its incoming values become literally ONE `Interval`, there's nothing left to "hint" toward; they're already unified. (This resolves one of the decomposition doc's own deferred questions: "the exact rule for deriving a φ-operand hint" — the answer is that the interval-merge already IS the strongest possible hint, no separate mechanism needed.)

### Fixed registers (the non-`Param`/`IntDiv`/`IntRem` half of bullet 10, populated here; eviction MECHANICS are 8b's job)

- `MachineInst::Param { dst, index }`: `fixed = Some(SYSV_INT_ARGS[class_relative_index])` or `SYSV_FLOAT_ARGS[class_relative_index]`, per `dst`'s `RegClass` and the class-relative-index counting described above. **Open question flagged for review, not resolved here**: what happens if `class_relative_index` exceeds the ABI register list's length (more than 6 int or 8 float params)? SysV spills extra arguments to the stack, a caller-side concern this project's parameter-passing story doesn't appear to address anywhere yet (searched CHECKLIST.md/SPEC.md/PROMPT.md — no mention of stack-passed arguments). Proposed resolution: `assert!` (loud failure) rather than silently mishandling, since this is genuinely out of scope for now and the whole codebase's examples are all small-arity expressions — but this needs the design reviewer's explicit sign-off, not a silent assumption.
- `MachineInst::IntDiv { dst, .. }`: `dst`'s interval gets `fixed = Some(PhysReg::Rax)` (the quotient's ABI-mandated location, per idiv's semantics and CHECKLIST bullet 10's literal wording).
- `MachineInst::IntRem { dst, .. }`: `dst`'s interval gets `fixed = Some(PhysReg::Rdx)` (the remainder's location).

**Open question flagged for review, NOT resolved here — the idiv "other register" clobber problem**: `idiv` clobbers BOTH `rax` and `rdx` regardless of which one the current instruction's `dst` claims (quotient always goes to rax, remainder always to rdx, and BOTH are destroyed even though `IntDiv`/`IntRem` are separate `MachineInst`s each only naming one). `Interval::fixed` as specified only pins `dst`'s OWN interval to ONE register — it has no way to express "also, whatever else might be live in the OTHER register at this exact instruction must be evicted, even though nothing here owns that register as `dst`." This is a real gap between SPEC.md's pseudocode-level `Interval` shape and idiv's true hardware semantics. Two candidate resolutions for review to pick between (not decided here): (a) add a `clobbers: SmallVec<[PhysReg; 2]>` field to `Interval` or a side-channel per-instruction clobber list that 8b's scan loop consults independently of `fixed`; (b) treat this as out of scope for 8a/8b and instead have the DEFERRED final-emission step insert an explicit save/restore of whichever register isn't the target around every `idiv`, the same way it already handles two-address-fixup copies — i.e., accept that Phase 8's allocator might occasionally colocate an unrelated live value in the "other" register across an idiv, and let emission-time save/restore (not register allocation) absorb the cost, rather than teaching the allocator a wholly new "instruction clobbers a register with no owning Value" concept for what's currently only 2 MachineInst variants. This design doc's author's own leaning is (b) (YAGNI — a full clobber-list mechanism is a lot of new surface area for two variants), but this must be an explicit, reviewed decision, not a silent default.

## Testing

- `RegClass::of` for all 3 `Ty` variants.
- `SYSV_INT_ARGS`/`SYSV_FLOAT_ARGS` contents match SPEC.md's documented lists.
- `block_starts` on a multi-block function (if/else) — confirms real block boundaries recovered, tested via a new assertion added to an existing `machine_inst/tests.rs` golden test plus at least one new test with a real branch.
- `build_intervals` on a single straight-line function (no branches) — golden `Vec<Interval>` test, confirming `start`/`end` positions match hand-traced expectations.
- `build_intervals` on an if/else function with a value live across the branch — confirms cross-block interval extension works (the value's `end` extends into the join block, past where it's textually last used in its OWN block).
- A φ-merge test: confirms a φ's interval and its incoming values' intervals become ONE `Interval` with the correct min-start/max-end.
- The critical-edge assertion: confirms it does NOT fire on any of this project's currently-producible if/else programs (a negative test — build a handful of real if/else `Function`s via the front-end/builder and confirm `build_intervals` never panics).
- Hint tests: a two-address-destructive op's `dst` gets `hint: Some(lhs_value)`; an op with no natural hint gets `hint: None`.
- `Param`'s class-relative fixed-register test: a function with mixed I64/F64/Bool params in a specific order, confirming each `Param`'s `fixed` register is the CLASS-relative one, not the raw `index`-based one (e.g. `f(x: f64, n: i64, y: f64)` → `x` gets `Xmm0`, `n` gets `Rdi`, `y` gets `Xmm1`, NOT `Xmm1`/`Rsi`/`Xmm2` from naively using `index` directly).
- `IntDiv`/`IntRem` fixed-register tests: `dst` gets `Rax`/`Rdx` respectively.

## Exit criteria

1. `crates/forge-regalloc/Cargo.toml` depends on `forge-ir` and `forge-x64`.
2. `SelectedFunction` gains a `block_starts: Vec<(Block, usize)>` field, populated correctly by `select()`, with no change to any other field or existing test's expected values.
3. `RegClass`, `SYSV_INT_ARGS`, `SYSV_FLOAT_ARGS` exist and are correct.
4. `Interval` struct exists, with `hint: Option<Value>` (a deliberate, documented deviation from SPEC.md's `Option<PhysReg>`).
5. `build_intervals(func: &Function, selected: &SelectedFunction) -> Vec<Interval>` exists, correctly computing `start`/`end` from real backward liveness dataflow (not an approximation), correctly merging φ intervals, correctly populating two-address hints, and correctly determining `fixed` for `Param`/`IntDiv`/`IntRem`.
6. The critical-edge tripwire assertion exists and never fires on any currently-producible program.
7. The idiv "other register" clobber question is EXPLICITLY decided (not silently defaulted) during design review, and the decision is reflected in this doc before planning begins.
8. The out-of-ABI-register-range `Param` question is EXPLICITLY decided during design review.
9. Tests cover every item in "Testing" above.
10. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
11. No regressions in any existing test, including `machine_inst/tests.rs`'s golden tests (which must still pass with `block_starts` added).
