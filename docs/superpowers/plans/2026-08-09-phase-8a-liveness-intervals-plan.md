# forge Phase 8a Liveness, Intervals, ABI Foundations & Hints Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational analysis layer for Phase 8 register allocation: extend `SelectedFunction` with block-boundary tracking, add `RegClass`/ABI constants/`Interval` to the new `forge-regalloc` crate, and implement `build_intervals(func, selected) -> Vec<Interval>` via real backward liveness dataflow, φ-interval merging (with a critical-edge tripwire), two-address hint population, and fixed/excluded-register determination for `Param`/`IntDiv`/`IntRem`.

**Architecture:** Six tasks. Task 1 touches Phase 7's already-shipped `crates/forge-x64` (an additive, backward-compatible field on `SelectedFunction`). Tasks 2-6 build entirely in `crates/forge-regalloc` (currently an empty stub). No register assignment happens anywhere in this plan — `build_intervals` only produces `Vec<Interval>`, ready for 8b to consume.

**Tech Stack:** Rust, `forge-ir`, `forge-x64`.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md` — read this first, in full. It has been through TWO rounds of execution-based review: the first found and fixed a design flaw (an over-engineered `IntDiv`/`IntRem` `lhs`-hint idea, corrected to a pure emission-time fixup); the second — applying THIS plan's exact code in a scratch worktree — found and fixed 5 real correctness bugs, the most serious being that **φ destinations silently got no `Interval` at all** in the plan's original draft (a value genuinely read later, e.g. by `Return`, would have gotten no register). All code below already reflects both rounds of fixes and has been confirmed to compile and pass every test — trust it.

**Interval range convention (important, corrects an earlier draft of the design doc)**: `[start, end]` is INCLUSIVE — `end` is the actual position of the value's last read, and the value is live AT that position. Two intervals `[0,2]` and `[2,4]` DO overlap. Any code comparing interval ranges (in this plan or in 8b/8d) must use `a.start <= b.end && b.start <= a.end`, never a half-open comparison.

---

## Task 1: Extend `SelectedFunction` with `block_starts`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-x64/src/machine_inst/tests.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/forge-x64/src/machine_inst/tests.rs`:

```rust
#[test]
fn select_records_block_starts_in_rpo_order() {
    // Same fixture shape as select_visits_blocks_in_true_rpo_not_creation_order:
    // an if/else with a join block, entry -> {then_block, else_block} -> join.
    let mut b = Builder::new();
    let entry = b.create_block();
    let then_block = b.create_block();
    let else_block = b.create_block();
    let join = b.create_block();
    b.add_pred(then_block, entry);
    b.add_pred(else_block, entry);
    b.add_pred(join, then_block);
    b.add_pred(join, else_block);
    b.seal_block(entry);

    let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
        cond,
        then_: then_block,
        else_: else_block,
    });

    b.seal_block(then_block);
    let then_val = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
    b.f.blocks[then_block.0 as usize].term = Some(Terminator::Jump(join));

    b.seal_block(else_block);
    let else_val = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
    b.f.blocks[else_block.0 as usize].term = Some(Terminator::Jump(join));

    b.seal_block(join);
    let phi = b.emit(
        join,
        Inst::Phi {
            incoming: smallvec::smallvec![(then_block, then_val), (else_block, else_val)],
        },
        Ty::I64,
        dummy_span(),
    );
    b.f.blocks[join.0 as usize].term = Some(Terminator::Return(phi));

    let selected = select(&b.f);

    // The order is REAL RPO, not creation order: dominance::reverse_postorder
    // does a DFS visiting `then_` before `else_` and reverses the postorder,
    // which puts else_block BEFORE then_block (the same "RPO != creation
    // order" property select_visits_blocks_in_true_rpo_not_creation_order
    // pins down for a straight chain).
    // Per-block MachineInst counts: entry = LoadImmI64(ConstBool) + Branch = 2;
    // else_block = LoadImmI64 + Jump = 2; then_block = LoadImmI64 + Jump = 2;
    // join = Phi (emits NOTHING, per Phase 7a) + Return = 1. Total 7.
    assert_eq!(
        selected.block_starts,
        vec![(entry, 0), (else_block, 2), (then_block, 4), (join, 6)]
    );
    assert_eq!(selected.insts.len(), 7);

    // Each recorded start must genuinely be that block's first instruction,
    // not just a plausible count: check the MachineInst actually sitting there.
    assert!(matches!(
        selected.insts[0],
        MachineInst::LoadImmI64 { dst, .. } if dst == cond
    ));
    assert!(matches!(
        selected.insts[2],
        MachineInst::LoadImmI64 { dst, .. } if dst == else_val
    ));
    assert!(matches!(
        selected.insts[4],
        MachineInst::LoadImmI64 { dst, .. } if dst == then_val
    ));
    assert!(matches!(selected.insts[6], MachineInst::Return { .. }));

    // Structural invariants the liveness pass depends on: entry is first,
    // starts are non-decreasing, and every start is a valid index.
    assert_eq!(selected.block_starts[0].0, b.f.entry);
    let positions: Vec<usize> = selected.block_starts.iter().map(|(_, pos)| *pos).collect();
    for w in positions.windows(2) {
        assert!(w[0] <= w[1], "block_starts positions must be non-decreasing");
    }
    assert!(*positions.last().unwrap() < selected.insts.len());
}
```

Also add this assertion to the EXISTING test `select_visits_blocks_in_true_rpo_not_creation_order`, right after its existing `assert_eq!(selected.insts, ...)`:

```rust
    // block_starts is recorded by the SAME walk, so it must agree with the
    // instruction order above: entry's Jump at 0, y's Jump at 1, x's two
    // instructions from 2.
    assert_eq!(selected.block_starts, vec![(entry, 0), (y, 1), (x, 2)]);
```

(Read the existing test first to confirm the exact block variable names `entry`/`y`/`x` match — adjust if the real names differ.)

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib -- select_records_block_starts_in_rpo_order select_visits_blocks_in_true_rpo_not_creation_order 2>&1 | tail -60`
Expected: FAIL — compile error (`selected.block_starts` field doesn't exist).

- [ ] **Step 3: Add the field and populate it**

`Block` is already imported at the top of `crates/forge-x64/src/machine_inst/mod.rs` (`use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};`) — no import change needed.

Add the field to `SelectedFunction`:

```rust
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    pub coalescing_hints: HashMap<Value, Value>,
    pub pool: ConstantPool,
    /// (Block, first-instruction-index-in-insts) for every block, in the
    /// same RPO order `insts` itself was built in. Lets later passes
    /// (Phase 8's liveness analysis) reconstruct block boundaries --
    /// `insts` alone has no boundary markers, and the IR-instruction-to-
    /// MachineInst count isn't 1:1 (Fma emits 2, Phi/suppressed lea
    /// operands emit 0), so only `select()`'s own walk can record this
    /// correctly. A block's end is the NEXT ENTRY'S start (by list
    /// position, not by searching for a larger value -- a block that
    /// selects to zero MachineInsts makes two consecutive entries share
    /// the same start, and only positional lookup gets that block's empty
    /// range right), or `insts.len()` for the last entry in this list.
    /// Only blocks reachable from `entry` appear here, since `select()`
    /// itself only walks `reverse_postorder(func)`.
    pub block_starts: Vec<(Block, usize)>,
}
```

Update `select()`'s body:

```rust
pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
        pool: ConstantPool::default(),
    };
    let mut block_starts = Vec::new();
    for block in forge_ir::dominance::reverse_postorder(func) {
        block_starts.push((block, sel.insts.len()));
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(term) = &func.blocks[block.0 as usize].term {
            sel.select_term(term);
        }
    }
    let coalescing_hints = compute_coalescing_hints(&sel.insts);
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
        coalescing_hints,
        pool: sel.pool,
        block_starts,
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -80`
Expected: both tests pass; ALL pre-existing tests still pass unchanged (this is purely additive — the ONE real `SelectedFunction { .. }` construction site is inside `select()` itself, which this step already updates).

- [ ] **Step 5: Run the FULL workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs crates/forge-x64/src/machine_inst/tests.rs
git commit -m "feat(forge-x64): SelectedFunction::block_starts, RPO block boundaries for liveness"
```

---

## Task 2: `forge-regalloc` scaffolding — `RegClass`, ABI constants, `Interval`

**Files:**
- Modify: `crates/forge-regalloc/Cargo.toml`
- Modify: `crates/forge-regalloc/src/lib.rs`
- Create: `crates/forge-regalloc/src/interval.rs`

- [ ] **Step 1: Add dependencies**

`crates/forge-regalloc/Cargo.toml` currently has an empty `[dependencies]` block. Change to:

```toml
[package]
name = "forge-regalloc"
version.workspace = true
edition.workspace = true

[dependencies]
forge-ir = { path = "../forge-ir" }
forge-x64 = { path = "../forge-x64" }

[dev-dependencies]
forge-syntax = { path = "../forge-syntax" }
smallvec.workspace = true
```

(The dev-dependencies are added now, up front, since Task 3 onward needs them for test fixtures — avoids a second `Cargo.toml` edit later.)

- [ ] **Step 2: Write `RegClass`, ABI constants, and `Interval` with their tests**

Create `crates/forge-regalloc/src/interval.rs`:

```rust
use forge_ir::{Ty, Value};
use forge_x64::PhysReg;

/// Which physical register file a value belongs in.
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

/// System V AMD64 integer/pointer argument registers, in order.
pub const SYSV_INT_ARGS: &[PhysReg] = &[
    PhysReg::Rdi,
    PhysReg::Rsi,
    PhysReg::Rdx,
    PhysReg::Rcx,
    PhysReg::R8,
    PhysReg::R9,
];

/// System V AMD64 float argument registers, in order.
pub const SYSV_FLOAT_ARGS: &[PhysReg] = &[
    PhysReg::Xmm0,
    PhysReg::Xmm1,
    PhysReg::Xmm2,
    PhysReg::Xmm3,
    PhysReg::Xmm4,
    PhysReg::Xmm5,
    PhysReg::Xmm6,
    PhysReg::Xmm7,
];

/// A virtual register's live range: `[start, end]` INCLUSIVE positions
/// into `SelectedFunction::insts` (the Vec index IS the linear
/// instruction number -- no separate numbering pass needed). `end` is the
/// value's last read position, and the value is live AT that position --
/// two intervals `[0,2]` and `[2,4]` DO overlap. `hint` points at another
/// Value this interval should try to share a physical location with, NOT
/// a bare PhysReg -- at construction time no value has been assigned a
/// real register yet (that's Phase 8b's job), so only a Value-to-Value
/// hint is meaningful here; 8b resolves it via its own scan-time
/// assignment map. This is a deliberate divergence from SPEC.md's
/// `Option<PhysReg>` sketch -- see the design doc's Hints section.
#[derive(Clone, Debug, PartialEq)]
pub struct Interval {
    pub value: Value,
    pub start: u32,
    pub end: u32,
    pub reg_class: RegClass,
    pub hint: Option<Value>,
    pub fixed: Option<PhysReg>,
    /// Always 0.0 in Phase 8a -- populated by Phase 8c's spill heuristic.
    pub spill_weight: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_class_of_maps_i64_and_bool_to_gpr_f64_to_xmm() {
        assert_eq!(RegClass::of(Ty::I64), RegClass::Gpr);
        assert_eq!(RegClass::of(Ty::Bool), RegClass::Gpr);
        assert_eq!(RegClass::of(Ty::F64), RegClass::Xmm);
    }

    #[test]
    fn sysv_int_args_matches_spec() {
        assert_eq!(
            SYSV_INT_ARGS,
            &[
                PhysReg::Rdi,
                PhysReg::Rsi,
                PhysReg::Rdx,
                PhysReg::Rcx,
                PhysReg::R8,
                PhysReg::R9
            ]
        );
    }

    #[test]
    fn sysv_float_args_matches_spec() {
        assert_eq!(
            SYSV_FLOAT_ARGS,
            &[
                PhysReg::Xmm0,
                PhysReg::Xmm1,
                PhysReg::Xmm2,
                PhysReg::Xmm3,
                PhysReg::Xmm4,
                PhysReg::Xmm5,
                PhysReg::Xmm6,
                PhysReg::Xmm7
            ]
        );
    }
}
```

- [ ] **Step 3: Wire the module into `lib.rs`**

Replace `crates/forge-regalloc/src/lib.rs`'s stub content entirely:

```rust
mod interval;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -40`
Expected: 3 tests pass.

- [ ] **Step 5: `cargo fmt` and `cargo clippy -p forge-regalloc --all-targets -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/Cargo.toml crates/forge-regalloc/src/lib.rs crates/forge-regalloc/src/interval.rs
git commit -m "feat(forge-regalloc): RegClass, SysV ABI arg-register constants, Interval"
```

---

## Task 3: Liveness dataflow

**Files:**
- Create: `crates/forge-regalloc/src/liveness.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-regalloc/src/liveness.rs` with only this content first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::builder::Builder;
    use forge_ir::{Inst, Terminator, Ty};
    use forge_syntax::span::Span;
    use forge_x64::select;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn straight_line_function_has_trivial_liveness() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let liveness = compute_liveness(&b.f, &selected);

        assert_eq!(liveness.live_in(entry), &std::collections::HashSet::new());
        assert_eq!(liveness.live_out(entry), &std::collections::HashSet::new());
    }

    #[test]
    fn value_live_across_a_branch_appears_in_live_out_of_the_defining_block() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_block = b.create_block();
        let else_block = b.create_block();
        b.add_pred(then_block, entry);
        b.add_pred(else_block, entry);
        b.seal_block(entry);

        let shared = b.emit(entry, Inst::ConstI64(7), Ty::I64, dummy_span());
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_block,
            else_: else_block,
        });

        b.seal_block(then_block);
        let one = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
        let then_result = b.emit(then_block, Inst::Add(shared, one), Ty::I64, dummy_span());
        b.f.blocks[then_block.0 as usize].term = Some(Terminator::Return(then_result));

        b.seal_block(else_block);
        let two = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        let else_result = b.emit(else_block, Inst::Add(shared, two), Ty::I64, dummy_span());
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Return(else_result));

        let selected = select(&b.f);
        let liveness = compute_liveness(&b.f, &selected);

        assert!(liveness.live_out(entry).contains(&shared));
        assert!(liveness.live_in(then_block).contains(&shared));
        assert!(liveness.live_in(else_block).contains(&shared));
        // cond, by contrast, is used inside entry (the Branch) and never
        // escapes it.
        assert!(!liveness.live_out(entry).contains(&cond));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- straight_line_function_has_trivial_liveness value_live_across_a_branch 2>&1 | tail -40`
Expected: FAIL — compile error (`compute_liveness`/`Liveness` don't exist yet).

- [ ] **Step 3: Write the implementation**

Prepend this to the TOP of `crates/forge-regalloc/src/liveness.rs`, above the `#[cfg(test)]` block from Step 1:

```rust
use forge_ir::{Block, Function, Value};
use forge_x64::{MachineInst, SelectedFunction};
use std::collections::{HashMap, HashSet};

/// Extracts the Value operands a MachineInst READS (not its dst, if any).
/// Exhaustive over every current MachineInst variant, no wildcard --
/// mirrors select_inst's own discipline: a newly-added MachineInst
/// variant must get a real arm here, or this fails to compile.
pub(crate) fn reads_of(inst: &MachineInst) -> Vec<Value> {
    match inst {
        MachineInst::LoadImmI64 { .. } | MachineInst::LoadImmF64 { .. } => vec![],
        MachineInst::IntAdd { lhs, rhs, .. }
        | MachineInst::IntSub { lhs, rhs, .. }
        | MachineInst::IntMul { lhs, rhs, .. }
        | MachineInst::IntDiv { lhs, rhs, .. }
        | MachineInst::IntRem { lhs, rhs, .. }
        | MachineInst::And { lhs, rhs, .. }
        | MachineInst::Or { lhs, rhs, .. }
        | MachineInst::Xor { lhs, rhs, .. }
        | MachineInst::Shl { lhs, rhs, .. }
        | MachineInst::Shr { lhs, rhs, .. }
        | MachineInst::Sar { lhs, rhs, .. }
        | MachineInst::FloatAdd { lhs, rhs, .. }
        | MachineInst::FloatSub { lhs, rhs, .. }
        | MachineInst::FloatMul { lhs, rhs, .. }
        | MachineInst::FloatDiv { lhs, rhs, .. }
        | MachineInst::FloatMin { lhs, rhs, .. }
        | MachineInst::FloatMax { lhs, rhs, .. } => vec![*lhs, *rhs],
        MachineInst::IntNeg { src, .. }
        | MachineInst::Not { src, .. }
        | MachineInst::FloatSqrt { src, .. }
        | MachineInst::FloatRound { src, .. }
        | MachineInst::FloatAbs { src, .. }
        | MachineInst::FloatNeg { src, .. }
        | MachineInst::IntToFloat { src, .. }
        | MachineInst::FloatToInt { src, .. } => vec![*src],
        MachineInst::Lea { base, index, .. } => vec![*base, *index],
        MachineInst::IntCmp { lhs, rhs, .. } | MachineInst::FloatCmp { lhs, rhs, .. } => {
            vec![*lhs, *rhs]
        }
        MachineInst::CallLibm { args, .. } => args.iter().copied().collect(),
        MachineInst::Jump { .. } => vec![],
        MachineInst::Branch { cond, .. } => vec![*cond],
        MachineInst::Return { value } => vec![*value],
        MachineInst::Param { .. } => vec![],
    }
}

/// Extracts the Value a MachineInst DEFINES, if any. Only the three
/// terminator variants (Jump/Branch/Return) define nothing -- everything
/// else, `Param` included, has a real `dst`. Same no-wildcard
/// exhaustiveness discipline as `reads_of` above.
pub(crate) fn def_of(inst: &MachineInst) -> Option<Value> {
    match inst {
        MachineInst::Jump { .. } | MachineInst::Branch { .. } | MachineInst::Return { .. } => {
            None
        }
        MachineInst::LoadImmI64 { dst, .. }
        | MachineInst::LoadImmF64 { dst, .. }
        | MachineInst::IntAdd { dst, .. }
        | MachineInst::IntSub { dst, .. }
        | MachineInst::IntMul { dst, .. }
        | MachineInst::IntDiv { dst, .. }
        | MachineInst::IntRem { dst, .. }
        | MachineInst::IntNeg { dst, .. }
        | MachineInst::And { dst, .. }
        | MachineInst::Or { dst, .. }
        | MachineInst::Xor { dst, .. }
        | MachineInst::Not { dst, .. }
        | MachineInst::Shl { dst, .. }
        | MachineInst::Shr { dst, .. }
        | MachineInst::Sar { dst, .. }
        | MachineInst::Lea { dst, .. }
        | MachineInst::FloatAdd { dst, .. }
        | MachineInst::FloatSub { dst, .. }
        | MachineInst::FloatMul { dst, .. }
        | MachineInst::FloatDiv { dst, .. }
        | MachineInst::FloatSqrt { dst, .. }
        | MachineInst::FloatMin { dst, .. }
        | MachineInst::FloatMax { dst, .. }
        | MachineInst::FloatRound { dst, .. }
        | MachineInst::FloatAbs { dst, .. }
        | MachineInst::FloatNeg { dst, .. }
        | MachineInst::IntCmp { dst, .. }
        | MachineInst::FloatCmp { dst, .. }
        | MachineInst::IntToFloat { dst, .. }
        | MachineInst::FloatToInt { dst, .. }
        | MachineInst::CallLibm { dst, .. }
        | MachineInst::Param { dst, .. } => Some(*dst),
    }
}

/// A block's instruction-index range within `SelectedFunction::insts`, by
/// its `block_starts` INDEX (every caller here already iterates
/// `block_starts`, so no by-`Block` lookup variant is needed).
///
/// Takes the NEXT ENTRY'S start as the end -- deliberately NOT "the first
/// entry with a larger start", which would mis-handle a block that
/// selects to zero MachineInsts (two adjacent entries sharing one start)
/// by handing that block its successor's instructions.
pub(crate) fn block_range_at(selected: &SelectedFunction, pos: usize) -> std::ops::Range<usize> {
    let start = selected.block_starts[pos].1;
    let end = selected
        .block_starts
        .get(pos + 1)
        .map(|(_, s)| *s)
        .unwrap_or(selected.insts.len());
    start..end
}

pub struct Liveness {
    live_in: HashMap<Block, HashSet<Value>>,
    live_out: HashMap<Block, HashSet<Value>>,
}

impl Liveness {
    pub fn live_in(&self, block: Block) -> &HashSet<Value> {
        &self.live_in[&block]
    }

    pub fn live_out(&self, block: Block) -> &HashSet<Value> {
        &self.live_out[&block]
    }
}

/// Standard backward per-block dataflow to a fixpoint:
///   live_out[B] = union of live_in[S] for each successor S of B
///   live_in[B]  = uses[B] union (live_out[B] minus defs[B])
/// `uses[B]` only counts a Value as used if it's read before any def of
/// the SAME value earlier in the same block (a value defined and used
/// within one block never needs to appear in that block's live_in).
///
/// `func` is unused directly (only blocks reachable via `select()`'s own
/// `reverse_postorder` walk appear in `block_starts`, and everything this
/// function needs about them comes from `selected`) -- kept as a
/// parameter for API symmetry with `build_intervals`, which DOES need it
/// (for phi handling, which lives entirely in the IR, not MachineInst).
pub fn compute_liveness(func: &Function, selected: &SelectedFunction) -> Liveness {
    let _ = func;
    let blocks: Vec<Block> = selected.block_starts.iter().map(|(b, _)| *b).collect();

    let mut uses: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut defs: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut successors: HashMap<Block, Vec<Block>> = HashMap::new();

    for (pos, &block) in blocks.iter().enumerate() {
        let range = block_range_at(selected, pos);
        let mut block_defs: HashSet<Value> = HashSet::new();
        let mut block_uses: HashSet<Value> = HashSet::new();
        let mut succs = Vec::new();
        for inst in &selected.insts[range] {
            for used in reads_of(inst) {
                if !block_defs.contains(&used) {
                    block_uses.insert(used);
                }
            }
            if let Some(d) = def_of(inst) {
                block_defs.insert(d);
            }
            match inst {
                MachineInst::Jump { target } => succs.push(*target),
                MachineInst::Branch { then_, else_, .. } => {
                    succs.push(*then_);
                    succs.push(*else_);
                }
                _ => {}
            }
        }
        uses.insert(block, block_uses);
        defs.insert(block, block_defs);
        successors.insert(block, succs);
    }

    let mut live_in: HashMap<Block, HashSet<Value>> =
        blocks.iter().map(|&b| (b, HashSet::new())).collect();
    let mut live_out: HashMap<Block, HashSet<Value>> =
        blocks.iter().map(|&b| (b, HashSet::new())).collect();

    let mut changed = true;
    while changed {
        changed = false;
        for &block in blocks.iter().rev() {
            let mut new_out = HashSet::new();
            for &succ in &successors[&block] {
                new_out.extend(live_in[&succ].iter().copied());
            }
            let mut new_in = uses[&block].clone();
            for v in new_out.difference(&defs[&block]) {
                new_in.insert(*v);
            }
            if new_in != live_in[&block] {
                live_in.insert(block, new_in);
                changed = true;
            }
            if new_out != live_out[&block] {
                live_out.insert(block, new_out);
                changed = true;
            }
        }
    }

    Liveness { live_in, live_out }
}
```

- [ ] **Step 4: Wire into `lib.rs`**

```rust
mod interval;
mod liveness;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
pub use liveness::{compute_liveness, Liveness};
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -60`
Expected: `straight_line_function_has_trivial_liveness` and `value_live_across_a_branch_appears_in_live_out_of_the_defining_block` both pass.

- [ ] **Step 6: `cargo fmt` and `cargo clippy -p forge-regalloc --all-targets -- -D warnings`, fix anything found**

`reads_of`/`def_of`/`block_range_at` are `pub(crate)` (used by Task 4's `intervals.rs`, not part of this crate's public API) — not `pub`.

- [ ] **Step 7: Commit**

```bash
git add crates/forge-regalloc/src/liveness.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): backward live_in/live_out liveness dataflow"
```

---

## Task 4: `build_intervals` — start/end from liveness, φ-merging, critical-edge tripwire

**Files:**
- Create: `crates/forge-regalloc/src/intervals.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-regalloc/src/intervals.rs` with only this content first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::builder::Builder;
    use forge_ir::{Function, Inst, Terminator, Ty};
    use forge_syntax::span::Span;
    use forge_x64::select;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    /// The real front-end pipeline, per `crates/forge-ir/tests/e2e.rs`:
    /// `lex` returns `(tokens, diags)`, `parse` takes TOKENS (not a &str)
    /// and returns `(ast, diags)`, `typecheck` takes an OWNED resolved
    /// `Ast` and returns `Result<TypedAst, Vec<Diagnostic>>`, and `lower`
    /// lives in the `forge_ir::lower` MODULE (`forge_ir::lower::lower`).
    /// VERIFY these exact names/signatures against the real
    /// crates/forge-ir/tests/e2e.rs before trusting this helper -- fix if
    /// the real API has since changed.
    fn front_end(src: &str) -> Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }

    #[test]
    fn straight_line_interval_starts_at_def_ends_at_last_use() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        // Selected layout: 0 Param(x), 1 LoadImmI64(one), 2 IntAdd(y), 3 Return(y).
        let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
        let one_iv = intervals.iter().find(|iv| iv.value == one).unwrap();
        let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
        assert_eq!((x_iv.start, x_iv.end), (0, 2));
        assert_eq!((one_iv.start, one_iv.end), (1, 2));
        // y is defined at the IntAdd and dies at the Return that reads it.
        assert_eq!((y_iv.start, y_iv.end), (2, 3));
        assert_eq!(x_iv.reg_class, RegClass::Gpr);
        assert_eq!(intervals.len(), 3);
    }

    #[test]
    fn value_live_across_a_branch_gets_an_interval_extending_into_the_successor() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_block = b.create_block();
        let else_block = b.create_block();
        b.add_pred(then_block, entry);
        b.add_pred(else_block, entry);
        b.seal_block(entry);

        let shared = b.emit(entry, Inst::ConstI64(7), Ty::I64, dummy_span());
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_block,
            else_: else_block,
        });

        b.seal_block(then_block);
        let one = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
        let then_result = b.emit(then_block, Inst::Add(shared, one), Ty::I64, dummy_span());
        b.f.blocks[then_block.0 as usize].term = Some(Terminator::Return(then_result));

        b.seal_block(else_block);
        let two = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        let else_result = b.emit(else_block, Inst::Add(shared, two), Ty::I64, dummy_span());
        // Each block returns ITS OWN result: returning `one` (defined in
        // then_block) from else_block would violate SSA def-dominates-use
        // and is not legal IR.
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Return(else_result));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        // RPO is entry, else_block, then_block (dominance::reverse_postorder
        // DFSes then_ first, then reverses), so the layout is:
        //   0 LoadImmI64(shared)  1 LoadImmI64(cond)  2 Branch
        //   3 LoadImmI64(two)     4 IntAdd(else_result) 5 Return
        //   6 LoadImmI64(one)     7 IntAdd(then_result) 8 Return
        assert_eq!(
            selected.block_starts,
            vec![(entry, 0), (else_block, 3), (then_block, 6)]
        );
        let shared_iv = intervals.iter().find(|iv| iv.value == shared).unwrap();
        // shared is used in BOTH successors; its interval must run from its
        // def all the way to its LAST use anywhere, well past entry's own
        // block boundary -- that is the whole point of running real
        // liveness instead of a per-block approximation.
        let entry_block_end = selected.block_starts[1].1;
        assert_eq!((shared_iv.start, shared_iv.end), (0, 7));
        assert!(shared_iv.end as usize > entry_block_end);
        // cond, by contrast, dies at the Branch inside entry.
        let cond_iv = intervals.iter().find(|iv| iv.value == cond).unwrap();
        assert_eq!((cond_iv.start, cond_iv.end), (1, 2));
    }

    #[test]
    fn phi_interval_merges_with_all_incoming_values() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_block = b.create_block();
        let else_block = b.create_block();
        let join = b.create_block();
        b.add_pred(then_block, entry);
        b.add_pred(else_block, entry);
        b.add_pred(join, then_block);
        b.add_pred(join, else_block);
        b.seal_block(entry);

        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_block,
            else_: else_block,
        });

        b.seal_block(then_block);
        let then_val = b.emit(then_block, Inst::ConstI64(1), Ty::I64, dummy_span());
        b.f.blocks[then_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(else_block);
        let else_val = b.emit(else_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(join);
        let phi = b.emit(
            join,
            Inst::Phi {
                incoming: smallvec::smallvec![(then_block, then_val), (else_block, else_val)],
            },
            Ty::I64,
            dummy_span(),
        );
        b.f.blocks[join.0 as usize].term = Some(Terminator::Return(phi));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        // Layout (RPO entry, else_block, then_block, join):
        //   0 LoadImmI64(cond)  1 Branch
        //   2 LoadImmI64(else_val)  3 Jump
        //   4 LoadImmI64(then_val)  5 Jump
        //   6 Return(phi)        <- the Phi itself emits NOTHING
        let then_iv = intervals.iter().find(|iv| iv.value == then_val).unwrap();
        let else_iv = intervals.iter().find(|iv| iv.value == else_val).unwrap();
        // The phi destination must HAVE an interval even though no
        // MachineInst defines it -- the Return genuinely reads it.
        let phi_iv = intervals
            .iter()
            .find(|iv| iv.value == phi)
            .expect("phi destination must get an interval of its own");
        // All three collapse to ONE range: earliest incoming def (else_val
        // at 2) through the phi's last use (the Return at 6).
        assert_eq!((then_iv.start, then_iv.end), (2, 6));
        assert_eq!((else_iv.start, else_iv.end), (2, 6));
        assert_eq!((phi_iv.start, phi_iv.end), (2, 6));
    }

    #[test]
    fn critical_edge_tripwire_never_fires_on_realistic_if_else_programs() {
        // Real front-end output for every if/else shape this project can
        // currently produce -- confirms build_intervals' critical-edge
        // assertion never fires on anything actually reachable today.
        for src in [
            "if x > 0.0 then x else 0.0 - x",
            "if a > b then a + b else a - b",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "if a > b then (if b > c then b else c) else (if a > c then a else c)",
            "let m = (if a > b then a else b) in m * m + sqrt(m)",
        ] {
            let func = front_end(src);
            let selected = select(&func);
            let _ = build_intervals(&func, &selected); // must not panic
        }
    }

    #[test]
    #[should_panic(expected = "critical edge")]
    fn critical_edge_tripwire_fires_on_a_hand_built_critical_edge() {
        // entry --Branch--> {a_block, join} and a_block --Jump--> join:
        // the entry->join edge is CRITICAL (entry has 2 successors, join has
        // 2 predecessors). No construct the front-end can produce today has
        // this shape -- it has to be built by hand -- but the tripwire must
        // genuinely detect it, not merely appear to.
        let mut b = Builder::new();
        let entry = b.create_block();
        let a_block = b.create_block();
        let join = b.create_block();
        b.add_pred(a_block, entry);
        b.add_pred(join, entry);
        b.add_pred(join, a_block);
        b.seal_block(entry);

        let c0 = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: a_block,
            else_: join,
        });

        b.seal_block(a_block);
        let c1 = b.emit(a_block, Inst::ConstI64(2), Ty::I64, dummy_span());
        b.f.blocks[a_block.0 as usize].term = Some(Terminator::Jump(join));

        b.seal_block(join);
        let phi = b.emit(
            join,
            Inst::Phi {
                incoming: smallvec::smallvec![(entry, c0), (a_block, c1)],
            },
            Ty::I64,
            dummy_span(),
        );
        b.f.blocks[join.0 as usize].term = Some(Terminator::Return(phi));

        let selected = select(&b.f);
        let _ = build_intervals(&b.f, &selected); // must panic
    }

    #[test]
    fn build_intervals_holds_its_invariants_across_the_whole_language_corpus() {
        // Every feature the front-end supports, incl. the shapes that make
        // selection non-1:1 with the IR: fma (2 MachineInsts + a synthetic
        // temp), libm calls, lea fusion (suppressed Mul/Shl), and phis
        // (0 MachineInsts).
        for src in [
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
        ] {
            let func = front_end(src);
            let selected = select(&func);
            let intervals = build_intervals(&func, &selected);

            for iv in &intervals {
                assert!(iv.start <= iv.end, "{src:?}: interval {iv:?} has end before start");
                assert!(
                    (iv.end as usize) < selected.insts.len(),
                    "{src:?}: interval {iv:?} runs past the end of insts"
                );
            }
            // Every Value any MachineInst reads or writes must have exactly
            // one interval -- a missing one means 8b would allocate no
            // register for a register the code genuinely needs.
            let covered: std::collections::HashSet<forge_ir::Value> =
                intervals.iter().map(|iv| iv.value).collect();
            assert_eq!(covered.len(), intervals.len(), "{src:?}: duplicate intervals");
            for inst in &selected.insts {
                for v in reads_of(inst).into_iter().chain(def_of(inst)) {
                    assert!(covered.contains(&v), "{src:?}: {v:?} has no interval");
                }
            }
        }
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- interval 2>&1 | tail -60`
Expected: FAIL — compile error (`build_intervals` doesn't exist yet).

- [ ] **Step 3: Write the implementation**

Prepend this to the TOP of `crates/forge-regalloc/src/intervals.rs`:

```rust
use crate::interval::{Interval, RegClass};
use crate::liveness::{block_range_at, compute_liveness, def_of, reads_of};
use forge_ir::{Block, Function, Inst, Terminator, Value};
use forge_x64::SelectedFunction;
use std::collections::HashMap;

/// Builds one `Interval` per virtual register the selected function
/// actually needs a physical location for -- every real SSA `Value` that
/// is defined by some `MachineInst`, plus every `Inst::Phi` destination
/// (which emits no `MachineInst` at all, yet is genuinely read by its
/// users), plus every synthetic temp the selector minted (Fma's `mul_tmp`,
/// typed via `selected.synthetic_types`).
///
/// `start`/`end` come from real backward liveness dataflow, not an
/// approximation: `end` covers every position a value is live across, not
/// just its last textual use inside its own block. φ destinations and all
/// their incoming values are then merged into one shared range (see
/// `merge_phi_intervals`), two-address hints are copied from
/// `SelectedFunction::coalescing_hints`, and `fixed` is populated for
/// `Param`/`IntDiv`/`IntRem`.
pub fn build_intervals(func: &Function, selected: &SelectedFunction) -> Vec<Interval> {
    let liveness = compute_liveness(func, selected);

    let mut start: HashMap<Value, u32> = HashMap::new();
    let mut end: HashMap<Value, u32> = HashMap::new();

    for (pos, &(block, block_start)) in selected.block_starts.iter().enumerate() {
        let range = block_range_at(selected, pos);

        // A phi emits NO MachineInst (Phase 7a), so its destination would
        // never be "defined" anywhere in `insts` and would silently end up
        // with no interval at all -- even though its users (a Return, an
        // IntAdd, ...) genuinely need it in a register. Seed those defs
        // here, at the top of the block that owns the phi, which is exactly
        // where a phi is conceptually defined.
        for &v in &func.blocks[block.0 as usize].insts {
            if matches!(func.insts[v.0 as usize], Inst::Phi { .. }) {
                start.entry(v).or_insert(block_start as u32);
                end.entry(v).or_insert(block_start as u32);
            }
        }

        // Anything live OUT of this block stays live through the block's
        // own last instruction, even when its last textual use sits in a
        // different block entirely -- this is what makes a flat [start, end]
        // range meaningful across block boundaries. (`insts` is laid out in
        // RPO, which is a topological order for today's DAG-shaped CFGs, so
        // a successor's positions are always greater than this block's.)
        if range.end > range.start {
            let block_last = (range.end - 1) as u32;
            for &v in liveness.live_out(block) {
                end.entry(v).and_modify(|e| *e = (*e).max(block_last)).or_insert(block_last);
            }
        }

        for (offset, inst) in selected.insts[range.clone()].iter().enumerate() {
            let p = (range.start + offset) as u32;
            for used in reads_of(inst) {
                end.entry(used).and_modify(|e| *e = (*e).max(p)).or_insert(p);
            }
            if let Some(d) = def_of(inst) {
                start.entry(d).or_insert(p);
                end.entry(d).or_insert(p);
            }
        }
    }

    let mut intervals: HashMap<Value, Interval> = HashMap::new();
    for (&v, &s) in &start {
        let ty = if (v.0 as usize) < func.types.len() {
            func.types[v.0 as usize]
        } else {
            selected.synthetic_types[&v]
        };
        let e = end.get(&v).copied().unwrap_or(s);
        // Both of these are IR-shape invariants, not allocator choices: a
        // value read with no reaching definition, or one whose last use
        // precedes its own definition, means the caller handed us IR that
        // `forge_ir::verify` would have rejected (SSA def-dominates-use).
        // Fail loudly rather than hand 8b a nonsense range.
        assert!(
            e >= s,
            "interval for {v:?} ends at {e} but starts at {s} -- its last use precedes its \
             definition, which means the input IR violates SSA def-dominates-use"
        );
        intervals.insert(
            v,
            Interval {
                value: v,
                start: s,
                end: e,
                reg_class: RegClass::of(ty),
                hint: None,
                fixed: None,
                spill_weight: 0.0,
            },
        );
    }
    for v in end.keys() {
        assert!(
            start.contains_key(v),
            "{v:?} is live but is never defined by any MachineInst (and isn't a phi \
             destination) -- it would get no interval and therefore no register at all"
        );
    }

    merge_phi_intervals(func, &mut intervals);
    populate_two_address_hints(selected, &mut intervals);
    populate_fixed_registers(func, selected, &mut intervals);

    intervals.into_values().collect()
}

/// A block's real successors, straight from its terminator -- the ground
/// truth of the CFG's edge set. (`BlockData::preds` also exists, but it is
/// `Builder`'s own SSA-construction bookkeeping, populated by explicit
/// `add_pred` calls a caller can forget, leave stale, or never make at all
/// for a hand-built `Function`; the terminators cannot disagree with the
/// CFG the selector actually laid out.)
fn successors_of(func: &Function, block: Block) -> Vec<Block> {
    match &func.blocks[block.0 as usize].term {
        Some(Terminator::Jump(t)) => vec![*t],
        Some(Terminator::Branch { then_, else_, .. }) => vec![*then_, *else_],
        Some(Terminator::Return(_)) | None => vec![],
    }
}

/// `Value -> owning Block`, for every value still present in some block's
/// instruction list. A value that has been dropped from its block (e.g. a
/// dead phi DCE removed from `block.insts` but left in `func.insts`) is
/// deliberately absent, and every caller here treats "absent" as "not part
/// of this function's real CFG, impose no constraint from it".
fn block_of_each_value(func: &Function) -> HashMap<Value, Block> {
    let mut owner = HashMap::new();
    for (i, bd) in func.blocks.iter().enumerate() {
        for &v in &bd.insts {
            owner.insert(v, Block(i as u32));
        }
    }
    owner
}

/// Re-verifies the critical-edge-free invariant Phase 7a's φ-lowering
/// depends on, then unions every φ destination with all of its incoming
/// values into ONE shared `[min start, max end]` range -- which is what
/// makes "φ emits nothing; its dst and its incoming values end up sharing
/// one physical location" real at the interval level.
///
/// ## The critical-edge tripwire
///
/// An edge `pred -> phi_block` is CRITICAL iff `pred` has more than one
/// successor AND `phi_block` has more than one predecessor, counting ALL
/// edges into it, not just this one. Across a critical edge the shared-
/// interval strategy is unsound (two predecessors reaching the φ block
/// would carry different values for the same φ, yet be forced into one
/// location with nowhere to insert the resolving copy), so this is an
/// `assert!`, not a `debug_assert!` -- matching this project's "invariant
/// bugs must fail loudly in release too" precedent (Phase 6a's `bind()`,
/// Phase 7d's `Rbp`-in-`callee_saved` guard).
///
/// Both counts come from real terminators (see `successors_of`): the
/// successor count of `pred`, and the total number of edges targeting
/// `phi_block` from anywhere in the function. Every program today's
/// front-end can produce is an if/else DAG whose φ blocks are only ever
/// reached by single-successor `Jump`s, so this can never fire yet; it
/// exists as a tripwire for whenever the front-end grows a construct that
/// could introduce one.
///
/// The merge itself uses union-find (not independent per-φ unions): a φ
/// can feed another φ, or two φs can share an incoming value, and merging
/// each φ independently in `func.insts` order would give order-dependent,
/// possibly-inconsistent ranges. Union-find collapses each connected
/// component into one shared range regardless of listing order.
fn merge_phi_intervals(func: &Function, intervals: &mut HashMap<Value, Interval>) {
    let owner = block_of_each_value(func);

    let mut pred_edge_counts = vec![0usize; func.blocks.len()];
    for i in 0..func.blocks.len() {
        for succ in successors_of(func, Block(i as u32)) {
            pred_edge_counts[succ.0 as usize] += 1;
        }
    }

    let mut parent: HashMap<Value, Value> = HashMap::new();
    let mut members: Vec<Value> = Vec::new();

    for (i, inst) in func.insts.iter().enumerate() {
        let Inst::Phi { incoming } = inst else {
            continue;
        };
        let phi_value = Value(i as u32);
        // A φ no longer present in any block imposes no constraint: it is
        // dead, contributes no MachineInst, and has no interval to merge.
        let Some(&phi_block) = owner.get(&phi_value) else {
            continue;
        };

        for &(pred_block, incoming_value) in incoming.iter() {
            let pred_successors = successors_of(func, pred_block).len();
            let phi_block_predecessors = pred_edge_counts[phi_block.0 as usize];
            assert!(
                pred_successors <= 1 || phi_block_predecessors <= 1,
                "critical edge {pred_block:?} -> {phi_block:?} feeding phi {phi_value:?} \
                 ({pred_block:?} has {pred_successors} successors, {phi_block:?} has \
                 {phi_block_predecessors} predecessors): Phase 7a's phi-lowering strategy \
                 (a phi and its incoming values sharing one interval) is unsound across a \
                 critical edge, and critical-edge splitting does not exist yet"
            );

            union(&mut parent, &mut members, phi_value, incoming_value);
        }
    }

    // One pass to collect each class's merged bounds, one to write them
    // back -- a member with no interval (a φ whose block never selected,
    // say) simply contributes nothing and receives nothing.
    let mut bounds: HashMap<Value, (u32, u32)> = HashMap::new();
    for &m in &members {
        let Some(iv) = intervals.get(&m) else {
            continue;
        };
        let root = find(&mut parent, m);
        let entry = bounds.entry(root).or_insert((iv.start, iv.end));
        entry.0 = entry.0.min(iv.start);
        entry.1 = entry.1.max(iv.end);
    }
    for &m in &members {
        let root = find(&mut parent, m);
        let Some(&(s, e)) = bounds.get(&root) else {
            continue;
        };
        if let Some(iv) = intervals.get_mut(&m) {
            iv.start = s;
            iv.end = e;
        }
    }
}

fn find(parent: &mut HashMap<Value, Value>, mut v: Value) -> Value {
    loop {
        let p = parent.get(&v).copied().unwrap_or(v);
        if p == v {
            return v;
        }
        let grandparent = parent.get(&p).copied().unwrap_or(p);
        parent.insert(v, grandparent); // path halving
        v = grandparent;
    }
}

fn union(parent: &mut HashMap<Value, Value>, members: &mut Vec<Value>, a: Value, b: Value) {
    for v in [a, b] {
        // `entry` rather than `contains_key` + `insert`: clippy::map_entry
        // is a default-on lint and rejects the two-lookup form under
        // `-D warnings`.
        if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(v) {
            slot.insert(v);
            members.push(v);
        }
    }
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent.insert(rb, ra);
    }
}

/// Two-address hints: for each `dst -> preferred_same_as` entry in
/// `SelectedFunction::coalescing_hints` (fully computed already, Phase 7b),
/// record `dst`'s interval hint as pointing at `preferred_same_as`. This is
/// a direct copy from an existing map, not new computation -- 8b resolves
/// the hinted `Value` to a real register via its own scan-time assignment
/// map.
fn populate_two_address_hints(
    selected: &SelectedFunction,
    intervals: &mut HashMap<Value, Interval>,
) {
    for (&dst, &preferred) in &selected.coalescing_hints {
        if let Some(iv) = intervals.get_mut(&dst) {
            iv.hint = Some(preferred);
        }
    }
}
```

Note: `populate_fixed_registers` is deliberately NOT included here — it's added in Task 5, along with its own tests. `build_intervals` above calls it, so Task 4's code will not compile in isolation until Task 5's `populate_fixed_registers` exists. Do Task 4 and Task 5 back-to-back if implementing without stopping (they were split for review/commit granularity, not because Task 4 is independently useful without Task 5).

- [ ] **Step 4: Wire into `lib.rs`**

```rust
mod interval;
mod intervals;
mod liveness;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
pub use intervals::build_intervals;
pub use liveness::{compute_liveness, Liveness};
```

- [ ] **Step 5: Proceed directly to Task 5 before attempting to compile/test**

Task 4's code alone will not compile (`populate_fixed_registers` is undefined). Continue to Task 5's Step 1-3, THEN run the combined test/compile step described in Task 5's Step 4.

---

## Task 5: Hints and fixed/excluded registers for `Param`/`IntDiv`/`IntRem`

**Files:**
- Modify: `crates/forge-regalloc/src/intervals.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/forge-regalloc/src/intervals.rs`'s existing `#[cfg(test)] mod tests` block (add `use forge_x64::PhysReg;` to the test module's `use` list, and add `use forge_ir::Value;` if not already present via the glob):

```rust
#[test]
fn two_address_op_dst_gets_hint_pointing_at_lhs() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
    let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
    let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
    assert_eq!(y_iv.hint, Some(x));
    // An op with no two-address constraint gets no hint at all.
    let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
    let one_iv = intervals.iter().find(|iv| iv.value == one).unwrap();
    assert_eq!(x_iv.hint, None);
    assert_eq!(one_iv.hint, None);
}

#[test]
fn mixed_type_params_get_class_relative_fixed_registers() {
    // f(x: f64, n: i64, y: f64) -- x is the 1st XMM arg (Xmm0), n is the
    // 1st GPR arg (Rdi) despite being the 2nd param overall, y is the 2nd
    // XMM arg (Xmm1) despite being the 3rd param overall.
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
    let n = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
    let y = b.emit(entry, Inst::Param { index: 2, ty: Ty::F64 }, Ty::F64, dummy_span());
    b.f.params = vec![
        ("x".to_string(), Ty::F64),
        ("n".to_string(), Ty::I64),
        ("y".to_string(), Ty::F64),
    ];
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(n));

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
    let n_iv = intervals.iter().find(|iv| iv.value == n).unwrap();
    let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
    assert_eq!(x_iv.fixed, Some(PhysReg::Xmm0));
    assert_eq!(n_iv.fixed, Some(PhysReg::Rdi));
    assert_eq!(y_iv.fixed, Some(PhysReg::Xmm1));
    assert_eq!(x_iv.reg_class, RegClass::Xmm);
    assert_eq!(n_iv.reg_class, RegClass::Gpr);
}

#[test]
fn params_are_found_by_scanning_not_by_assuming_value_index_equals_param_index() {
    // The same three params, but with two unrelated instructions emitted
    // BEFORE them, so Value(0)/Value(1)/Value(2) are NOT the params.
    // Assuming Value(index) == the param's own Value would silently pin
    // the ABI registers onto the wrong values here (and leave the real
    // params unfixed).
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let junk_a = b.emit(entry, Inst::ConstI64(11), Ty::I64, dummy_span());
    let junk_b = b.emit(entry, Inst::ConstI64(22), Ty::I64, dummy_span());
    let p0 = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
    let p1 = b.emit(entry, Inst::Param { index: 1, ty: Ty::F64 }, Ty::F64, dummy_span());
    b.f.params = vec![("p0".to_string(), Ty::I64), ("p1".to_string(), Ty::F64)];
    let sum = b.emit(entry, Inst::Add(junk_a, junk_b), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let iv = |v: Value| intervals.iter().find(|iv| iv.value == v).unwrap().clone();
    assert_eq!(iv(p0).fixed, Some(PhysReg::Rdi));
    assert_eq!(iv(p1).fixed, Some(PhysReg::Xmm0));
    assert_eq!(iv(junk_a).fixed, None);
    assert_eq!(iv(junk_b).fixed, None);
}

#[test]
#[should_panic(expected = "more than 6 integer/bool parameters")]
fn seventh_int_param_panics() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let mut last = None;
    let mut params = Vec::new();
    for i in 0..7u32 {
        let v = b.emit(entry, Inst::Param { index: i, ty: Ty::I64 }, Ty::I64, dummy_span());
        params.push((format!("p{i}"), Ty::I64));
        last = Some(v);
    }
    b.f.params = params;
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(last.unwrap()));

    let selected = select(&b.f);
    let _ = build_intervals(&b.f, &selected); // must panic
}

#[test]
fn int_div_dst_fixed_rax_int_rem_dst_fixed_rdx() {
    // The dividend is deliberately NOT a Param: a Param would pick up a
    // fixed ABI register from the Param rule, masking the property this
    // test is about (idiv's DIVIDEND gets no allocator-level treatment of
    // its own -- the design's "pure emission-time fixup" resolution).
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let a = b.emit(entry, Inst::ConstI64(100), Ty::I64, dummy_span());
    let c = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
    let q = b.emit(entry, Inst::Div(a, c), Ty::I64, dummy_span());
    let r = b.emit(entry, Inst::Rem(a, c), Ty::I64, dummy_span());
    let sum = b.emit(entry, Inst::Add(q, r), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let q_iv = intervals.iter().find(|iv| iv.value == q).unwrap();
    let r_iv = intervals.iter().find(|iv| iv.value == r).unwrap();
    let a_iv = intervals.iter().find(|iv| iv.value == a).unwrap();
    assert_eq!(q_iv.fixed, Some(PhysReg::Rax));
    assert_eq!(r_iv.fixed, Some(PhysReg::Rdx));
    // lhs (a) gets NO special treatment at the Interval level -- neither a
    // fixed register nor a hint -- confirming the design's "pure
    // emission-time fixup, no allocator-level hint" resolution.
    assert_eq!(a_iv.fixed, None);
    assert_eq!(a_iv.hint, None);
    // ...and IntDiv/IntRem contribute no coalescing hint for their dst
    // either (compute_coalescing_hints deliberately excludes them).
    assert_eq!(q_iv.hint, None);
    assert_eq!(r_iv.hint, None);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- two_address_op_dst mixed_type_params params_are_found seventh_int_param int_div_dst 2>&1 | tail -80`
Expected: FAIL — compile error (`populate_fixed_registers`/`param_value_by_index` don't exist yet, so `build_intervals` itself doesn't compile).

- [ ] **Step 3: Implement**

Append to `crates/forge-regalloc/src/intervals.rs` (before the `#[cfg(test)]` module):

```rust
/// `Param`'s class-relative ABI register, and `IntDiv`/`IntRem`'s fixed
/// rax/rdx `dst` -- see the design doc's "Fixed registers" section for the
/// full reasoning (in particular why `rhs`/`lhs` get NO Interval-level
/// treatment here: `rhs` is handled by `excluded_registers` in Task 6,
/// `lhs` by emission-time copy insertion, out of this crate's scope).
fn populate_fixed_registers(
    func: &Function,
    selected: &SelectedFunction,
    intervals: &mut HashMap<Value, Interval>,
) {
    // `Inst::Param { index, .. }`'s `index` counts ALL parameters
    // regardless of type, but SysV assigns integer and float arguments
    // from SEPARATE register files -- so the register a param lands in is
    // determined by how many EARLIER params share its RegClass, not by
    // `index`. `func.params` is the authority on declaration order/types
    // (it is 1:1 with `index` by construction in `lower.rs`).
    let param_values = param_value_by_index(func);
    let mut gpr_seen = 0usize;
    let mut xmm_seen = 0usize;
    for (index, &(_, ty)) in func.params.iter().enumerate() {
        // The class counters must advance for EVERY declared parameter,
        // including one whose defining instruction is gone (dead-code
        // eliminated) or was never emitted -- the ABI register a LATER
        // param occupies depends on the full declared list, not on which
        // params survived.
        let value = param_values.get(&(index as u32)).copied();
        match RegClass::of(ty) {
            RegClass::Gpr => {
                assert!(
                    gpr_seen < crate::interval::SYSV_INT_ARGS.len(),
                    "function has more than {} integer/bool parameters -- exceeds SysV's \
                     integer argument register count; this needs to become a real Diagnostic \
                     before any user-facing CLI surface ships (tracked in the Phase 8a design doc)",
                    crate::interval::SYSV_INT_ARGS.len()
                );
                if let Some(iv) = value.and_then(|v| intervals.get_mut(&v)) {
                    iv.fixed = Some(crate::interval::SYSV_INT_ARGS[gpr_seen]);
                }
                gpr_seen += 1;
            }
            RegClass::Xmm => {
                assert!(
                    xmm_seen < crate::interval::SYSV_FLOAT_ARGS.len(),
                    "function has more than {} float parameters -- exceeds SysV's float \
                     argument register count; this needs to become a real Diagnostic before \
                     any user-facing CLI surface ships (tracked in the Phase 8a design doc)",
                    crate::interval::SYSV_FLOAT_ARGS.len()
                );
                if let Some(iv) = value.and_then(|v| intervals.get_mut(&v)) {
                    iv.fixed = Some(crate::interval::SYSV_FLOAT_ARGS[xmm_seen]);
                }
                xmm_seen += 1;
            }
        }
    }

    for inst in &selected.insts {
        match inst {
            forge_x64::MachineInst::IntDiv { dst, .. } => {
                if let Some(iv) = intervals.get_mut(dst) {
                    iv.fixed = Some(forge_x64::PhysReg::Rax);
                }
            }
            forge_x64::MachineInst::IntRem { dst, .. } => {
                if let Some(iv) = intervals.get_mut(dst) {
                    iv.fixed = Some(forge_x64::PhysReg::Rdx);
                }
            }
            _ => {}
        }
    }
}

/// Maps each parameter index to the `Value` its `Inst::Param` defines, by
/// scanning `func.insts` for real `Param` instructions.
///
/// Deliberately NOT `Value(index)`: that shortcut happens to hold for a
/// function straight out of `lower.rs` (which emits every `Param` first,
/// into the entry block, and `Builder::emit` numbers values by
/// `f.insts.len()`), but nothing enforces it -- later passes can append
/// new instructions freely, and a hand-built `Function` can set `params`
/// without emitting `Param` instructions at index 0 at all. Scanning is
/// the same cost and can't go stale.
fn param_value_by_index(func: &Function) -> HashMap<u32, Value> {
    let mut by_index = HashMap::new();
    for (i, inst) in func.insts.iter().enumerate() {
        if let Inst::Param { index, .. } = inst {
            by_index.insert(*index, Value(i as u32));
        }
    }
    by_index
}
```

- [ ] **Step 4: Run the tests (this compiles Task 4 + Task 5 together)**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -100`
Expected: all Task 4 and Task 5 tests pass (11 tests total: 4 from Task 4's Step 1 + 5 from Task 5's Step 1, plus the pre-existing scaffolding/liveness tests).

- [ ] **Step 5: Run the FULL workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -80`

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-regalloc/src/intervals.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): build_intervals with liveness, phi-merging via union-find, critical-edge tripwire, two-address hints, Param/IntDiv/IntRem fixed-register population"
```

---

## Task 6: `rhs`-exclusion side channel for `IntDiv`/`IntRem`

**Files:**
- Modify: `crates/forge-regalloc/src/intervals.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `intervals.rs`'s test module:

```rust
#[test]
fn int_div_rhs_is_excluded_from_rax_and_rdx() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let a = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
    let c = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
    let q = b.emit(entry, Inst::Div(a, c), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(q));

    let selected = select(&b.f);
    let excluded = excluded_registers(&b.f, &selected);

    // c (rhs of the Div) must be excluded from Rax/Rdx AT the IntDiv
    // instruction's position specifically.
    let div_pos = selected
        .insts
        .iter()
        .position(|i| matches!(i, forge_x64::MachineInst::IntDiv { .. }))
        .unwrap();
    let excl = excluded.get(&(div_pos, c)).expect("c must have an exclusion entry at div_pos");
    assert!(excl.contains(&PhysReg::Rax));
    assert!(excl.contains(&PhysReg::Rdx));
    // The dividend is NOT excluded -- emission fixes it with a copy.
    assert!(!excluded.contains_key(&(div_pos, a)));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- int_div_rhs_is_excluded 2>&1 | tail -40`
Expected: FAIL — compile error (`excluded_registers` doesn't exist).

- [ ] **Step 3: Implement**

Append to `intervals.rs` (before the `#[cfg(test)]` module):

```rust
/// Per-(instruction position, Value) register exclusions -- currently
/// only populated for IntDiv/IntRem's rhs (divisor), which must never be
/// assigned Rax or Rdx (cqo/idiv would destroy it before idiv ever reads
/// it -- see the design doc's idiv-clobber resolution, sub-problem 2).
/// 8b's design doc consumes this as an extra candidate-set filter in
/// pick_register, on top of ordinary availability.
///
/// `func` is unused directly (everything needed is in `selected.insts`) --
/// kept as a parameter for API symmetry with `build_intervals`.
pub fn excluded_registers(
    func: &Function,
    selected: &SelectedFunction,
) -> HashMap<(usize, Value), Vec<forge_x64::PhysReg>> {
    let _ = func;
    let mut excluded = HashMap::new();
    for (i, inst) in selected.insts.iter().enumerate() {
        match inst {
            forge_x64::MachineInst::IntDiv { rhs, .. }
            | forge_x64::MachineInst::IntRem { rhs, .. } => {
                excluded.insert((i, *rhs), vec![forge_x64::PhysReg::Rax, forge_x64::PhysReg::Rdx]);
            }
            _ => {}
        }
    }
    excluded
}
```

- [ ] **Step 4: Wire into `lib.rs`**

```rust
mod interval;
mod intervals;
mod liveness;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
pub use intervals::{build_intervals, excluded_registers};
pub use liveness::{compute_liveness, Liveness};
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -60`
Expected: 18 tests pass (3 scaffolding + 2 liveness + 4 build_intervals + 5 hints/fixed + 1 rhs-exclusion + the corpus test + the two critical-edge tests + params-by-scanning).

- [ ] **Step 6: Run the FULL workspace test suite, fmt, clippy**

```bash
cargo test --workspace
cargo fmt
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/forge-regalloc/src/intervals.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): rhs register-exclusion side channel for IntDiv/IntRem"
```

---

## Task 7: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -80`. Expect approximately 385 tests total across the workspace (per this plan's own execution-based review), with `forge-regalloc` contributing ~18 and `forge-x64` lib contributing 95 (up from 94 pre-Task-1). Report exact final counts.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings` AND `cargo clippy --workspace --all-targets -- -D warnings` (both — the non-`--all-targets` form can miss warnings that only appear in test code, which bit an earlier review pass of this same plan).

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm exit criteria 1-11 from `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md`'s "Exit criteria" section. Criterion 12 (retaining `Interval`/assignment data forward past 8b/8c) is NOT this task's or this plan's job — it's a requirement on 8b/8c's own future design docs, structurally unaddressable within 8a alone since nothing downstream exists yet.

## Context for this whole plan

This plan was reviewed with TWO rounds of execution-based verification (a scratch worktree, real `cargo build`/`test`/`clippy`/`fmt`, not just reading). The second round found and fixed 5 real bugs in an earlier draft — most seriously, φ destinations getting no `Interval` at all (a value genuinely read later would have silently gotten no register). Every code block above already reflects those fixes and has been confirmed to compile and pass. If something still doesn't compile or pass as written, suspect a transcription slip in this plan before suspecting the underlying approach.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`
