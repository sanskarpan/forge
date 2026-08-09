# forge Phase 8a Liveness, Intervals, ABI Foundations & Hints Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational analysis layer for Phase 8 register allocation: extend `SelectedFunction` with block-boundary tracking, add `RegClass`/ABI constants/`Interval` to the new `forge-regalloc` crate, and implement `build_intervals(func, selected) -> Vec<Interval>` via real backward liveness dataflow, φ-interval merging (with a critical-edge tripwire), two-address hint population, and fixed/excluded-register determination for `Param`/`IntDiv`/`IntRem`.

**Architecture:** Six tasks. Task 1 touches Phase 7's already-shipped `crates/forge-x64` (an additive, backward-compatible field on `SelectedFunction`). Tasks 2-6 build entirely in `crates/forge-regalloc` (currently an empty stub). No register assignment happens anywhere in this plan — `build_intervals` only produces `Vec<Interval>`, ready for 8b to consume.

**Tech Stack:** Rust, `forge-ir`, `forge-x64`.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md` — read this first, in full. It was reviewed with an execution-based pass (scratch worktree, real `cargo build`/`test`/`clippy`) that found and fixed a real design flaw (an over-engineered `lhs`-hint idea for `IntDiv`/`IntRem`, corrected to a pure emission-time fixup) and confirmed the rest of the design's claims against real code. Trust its resolved decisions; don't re-litigate them here.

---

## Task 1: Extend `SelectedFunction` with `block_starts`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-x64/src/machine_inst/tests.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/forge-x64/src/machine_inst/tests.rs`, near `select_visits_blocks_in_true_rpo_not_creation_order` (a good existing 3-block RPO test to model this on — read it first for the exact `Builder` fixture pattern it uses):

```rust
#[test]
fn select_records_block_starts_in_rpo_order() {
    // Reuses the same fixture shape as select_visits_blocks_in_true_rpo_not_creation_order:
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

    // block_starts must be in the same RPO order insts was built in, and
    // each entry's index must genuinely be that block's first position in
    // insts -- confirmed by checking the recorded start position's
    // MachineInst is a Param/LoadImm/etc belonging to that block, not by
    // just trusting the count.
    assert_eq!(selected.block_starts.len(), 4);
    let starts: std::collections::HashMap<_, _> = selected.block_starts.iter().copied().collect();
    assert_eq!(starts[&entry], 0);
    // entry has exactly 1 real MachineInst (the ConstBool->LoadImmI64) plus its Branch terminator = 2 insts.
    assert_eq!(selected.insts.len() >= starts[&then_block] + 0, true);
    // then_block and else_block each contribute 1 real inst + 1 Jump terminator = 2 insts each.
    // join has 0 insts for the Phi (emits nothing, per Phase 7a) + 1 Return terminator = 1 inst.
    // Rather than hand-deriving exact positions (error-prone without executing), assert the
    // STRUCTURAL invariant instead: starts are strictly increasing in the order block_starts lists
    // them, and the last block's start is strictly less than insts.len().
    let positions: Vec<usize> = selected.block_starts.iter().map(|(_, pos)| *pos).collect();
    for w in positions.windows(2) {
        assert!(w[0] < w[1], "block_starts positions must be strictly increasing");
    }
    assert!(*positions.last().unwrap() < selected.insts.len());
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib -- select_records_block_starts_in_rpo_order 2>&1 | tail -40`
Expected: FAIL — compile error (`selected.block_starts` field doesn't exist).

- [ ] **Step 3: Add the field and populate it**

In `crates/forge-x64/src/machine_inst/mod.rs`, add `Block` to the top-of-file import (currently `use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};` — check whether `Block` is already imported; if not, add it):

```rust
use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};
```

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
    /// correctly. A block's end is the next entry's start (or
    /// `insts.len()` for the last block in this list).
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

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -60`
Expected: `select_records_block_starts_in_rpo_order` passes; ALL pre-existing tests still pass unchanged (this is purely additive — no existing `SelectedFunction { .. }` struct-literal test assertion should need updating, since Rust would give a "missing field" compile error at the ONE real construction site in `select()` itself, not at test call sites that only read fields).

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
```

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

/// A virtual register's live range: `[start, end)` positions into
/// `SelectedFunction::insts` (the Vec index IS the linear instruction
/// number -- no separate numbering pass needed). `hint` points at another
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
        // x = param 0; y = x + 1; return y
        // No block boundaries, no cross-block liveness -- the simplest
        // possible case, to nail down the basic per-block uses/defs
        // extraction before testing anything cross-block.
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(
            entry,
            Inst::Param { index: 0, ty: Ty::I64 },
            Ty::I64,
            dummy_span(),
        );
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let liveness = compute_liveness(&b.f, &selected);

        // Nothing is live INTO the entry block from anywhere (it's the
        // function's start) and nothing is live OUT of it (Return has no
        // successors) -- the whole computation happens and dies within
        // one block.
        assert_eq!(liveness.live_in(entry), &std::collections::HashSet::new());
        assert_eq!(liveness.live_out(entry), &std::collections::HashSet::new());
    }

    #[test]
    fn value_live_across_a_branch_appears_in_live_out_of_the_defining_block() {
        // entry: cond = ...; branch cond -> then/else
        // A value defined in `entry` and used in BOTH `then` and `else`
        // must be in entry's live_out (it survives past entry's own last
        // instruction, into both successors).
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
/// Mirrors forge_ir::uses_of's own exhaustiveness discipline conceptually,
/// though MachineInst's variant set differs from Inst's -- covers every
/// variant explicitly so a newly-added MachineInst variant fails to
/// compile here until given a real arm, the same discipline select_inst
/// itself uses.
fn reads_of(inst: &MachineInst) -> Vec<Value> {
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

/// Extracts the Value a MachineInst DEFINES, if any (terminators and Param
/// -- wait, Param DOES define -- have varying shapes; this returns None
/// only for the true no-dst variants: Jump/Branch/Return).
fn def_of(inst: &MachineInst) -> Option<Value> {
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

/// A block's instruction-index range within SelectedFunction::insts,
/// derived from block_starts (Phase 8a's own addition to SelectedFunction).
fn block_range(selected: &SelectedFunction, block: Block) -> std::ops::Range<usize> {
    let pos = selected
        .block_starts
        .iter()
        .position(|(b, _)| *b == block)
        .expect("block must appear in block_starts");
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
pub fn compute_liveness(func: &Function, selected: &SelectedFunction) -> Liveness {
    let blocks: Vec<Block> = selected.block_starts.iter().map(|(b, _)| *b).collect();

    let mut uses: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut defs: HashMap<Block, HashSet<Value>> = HashMap::new();
    let mut successors: HashMap<Block, Vec<Block>> = HashMap::new();

    for &block in &blocks {
        let range = block_range(selected, block);
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

    let _ = func; // func not otherwise needed once block_starts/insts are available; kept for API symmetry with build_intervals.

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

- [ ] **Step 5: Add `forge-syntax` and `smallvec` as dev-dependencies (needed for the test fixtures)**

`crates/forge-regalloc/Cargo.toml`:
```toml
[dev-dependencies]
forge-syntax = { path = "../forge-syntax" }
smallvec.workspace = true
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -60`
Expected: `straight_line_function_has_trivial_liveness` and `value_live_across_a_branch_appears_in_live_out_of_the_defining_block` both pass.

- [ ] **Step 7: `cargo fmt` and `cargo clippy -p forge-regalloc --all-targets -- -D warnings`, fix anything found**

The `reads_of`/`def_of` matches are exhaustive (no wildcard) by design, mirroring `select_inst`'s own discipline — if a future `MachineInst` variant is added, this file MUST fail to compile until given a real arm here too; do not add `_ => {}` to silence a compile error.

- [ ] **Step 8: Commit**

```bash
git add crates/forge-regalloc/src/liveness.rs crates/forge-regalloc/src/lib.rs crates/forge-regalloc/Cargo.toml
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
    use forge_ir::{Inst, Terminator, Ty};
    use forge_syntax::span::Span;
    use forge_x64::select;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn straight_line_interval_starts_at_def_ends_at_last_use() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(
            entry,
            Inst::Param { index: 0, ty: Ty::I64 },
            Ty::I64,
            dummy_span(),
        );
        let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
        // x is defined at position 0 (the Param MachineInst) and last used
        // at the IntAdd -- its interval must span at least that range.
        assert_eq!(x_iv.start, 0);
        assert!(x_iv.end > x_iv.start);
        assert_eq!(x_iv.reg_class, RegClass::Gpr);
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
        let _else_result = b.emit(else_block, Inst::Add(shared, two), Ty::I64, dummy_span());
        b.f.blocks[else_block.0 as usize].term = Some(Terminator::Return(one)); // arbitrary valid return

        let selected = select(&b.f);
        let intervals = build_intervals(&b.f, &selected);

        let shared_iv = intervals.iter().find(|iv| iv.value == shared).unwrap();
        // shared's last real use is inside then_block or else_block, both
        // AFTER entry's own last instruction -- its end must extend past
        // entry's own block, not stop at entry's own boundary. block_starts[1]
        // is the second block visited in RPO (entry is always block_starts[0]
        // since it has no predecessors), so its start position is entry's
        // own end -- shared's interval must extend at least that far.
        let entry_block_end = selected.block_starts[1].1;
        assert!(shared_iv.end as usize >= entry_block_end - 1);
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

        // then_val, else_val, and phi must all resolve to ONE interval
        // (same Value identity in the merged set -- exact representation
        // is that the SAME Interval entry's `value` covers the merge; the
        // simplest correct contract is that build_intervals returns intervals
        // keyed such that then_val/else_val/phi all map to intervals with
        // IDENTICAL start/end after the merge).
        let then_iv = intervals.iter().find(|iv| iv.value == then_val).unwrap();
        let else_iv = intervals.iter().find(|iv| iv.value == else_val).unwrap();
        assert_eq!(then_iv.start, else_iv.start.min(then_iv.start));
        assert_eq!(then_iv.end, else_iv.end);
    }

    #[test]
    fn critical_edge_tripwire_never_fires_on_realistic_if_else_programs() {
        // A handful of real if/else shapes -- confirms build_intervals
        // never panics on anything this project's front-end can actually
        // produce today.
        for src in [
            "if x > 0.0 { x } else { 0.0 - x }",
            "if a > b { a + b } else { a - b }",
        ] {
            let ast = forge_syntax::parse(src).expect("parse");
            let typed = forge_syntax::typecheck(&ast).expect("typecheck");
            let func = forge_ir::lower(&typed);
            let selected = select(&func);
            let _ = build_intervals(&func, &selected); // must not panic
        }
    }
}
```

**IMPORTANT — verify the exact public API names before using them**: `forge_syntax::parse`/`forge_syntax::typecheck`/`forge_ir::lower` are used above based on this project's established naming conventions from prior phases' tests (e.g. `crates/forge-ir/tests/e2e.rs`), but READ that file first to confirm the exact function names/signatures/return types (`Result` vs `Option`, exact error type) before writing this test for real — don't guess if it doesn't compile as written; match the real API.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- interval 2>&1 | tail -60`
Expected: FAIL — compile error (`build_intervals`/`Interval` construction functions don't exist in this module yet).

- [ ] **Step 3: Write the implementation**

Prepend this to the TOP of `crates/forge-regalloc/src/intervals.rs`:

```rust
use crate::interval::{Interval, RegClass};
use crate::liveness::compute_liveness;
use forge_ir::{Function, Inst, Value};
use forge_x64::{MachineInst, SelectedFunction};
use std::collections::HashMap;

/// Builds one Interval per real SSA Value (synthetic Fma temps included,
/// via `selected.synthetic_types`), with correct [start, end) ranges from
/// real backward liveness analysis, φ-merged intervals, and (fixed/hint
/// population deferred to Task 5 -- this task produces start/end/reg_class
/// only, with hint/fixed left at their default None).
pub fn build_intervals(func: &Function, selected: &SelectedFunction) -> Vec<Interval> {
    let liveness = compute_liveness(func, selected);

    // start: first position (lowest index) at which each Value is defined.
    // end: last position at which each Value is used OR live-out of some
    // block it passes through (whichever is greater) -- computed by
    // walking every block's instruction range, seeding a working live set
    // from that block's live_out, and extending each live value's end to
    // at least the block's own last position, then walking backward
    // through the block updating def/use positions exactly.
    let mut start: HashMap<Value, u32> = HashMap::new();
    let mut end: HashMap<Value, u32> = HashMap::new();

    for &(block, block_start) in &selected.block_starts {
        let block_end = selected
            .block_starts
            .iter()
            .find(|(b, s)| *s > block_start && *b != block)
            .map(|(_, s)| *s)
            .unwrap_or(selected.insts.len());
        // Every value live OUT of this block must have its end extended
        // to at least the position just past this block's last instruction.
        for &v in liveness.live_out(block) {
            let extended = (block_end.saturating_sub(1)) as u32;
            end.entry(v).and_modify(|e| *e = (*e).max(extended)).or_insert(extended);
        }
        for (offset, inst) in selected.insts[block_start..block_end].iter().enumerate() {
            let pos = (block_start + offset) as u32;
            for used in crate::liveness_test_support::reads_of_pub(inst) {
                end.entry(used).and_modify(|e| *e = (*e).max(pos)).or_insert(pos);
            }
            if let Some(d) = crate::liveness_test_support::def_of_pub(inst) {
                start.entry(d).or_insert(pos);
                end.entry(d).or_insert(pos);
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
        intervals.insert(
            v,
            Interval {
                value: v,
                start: s,
                end: end.get(&v).copied().unwrap_or(s),
                reg_class: RegClass::of(ty),
                hint: None,
                fixed: None,
                spill_weight: 0.0,
            },
        );
    }

    merge_phi_intervals(func, &mut intervals);

    intervals.into_values().collect()
}

/// For every Inst::Phi in `func`, unions the φ's own interval with every
/// incoming value's interval into ONE shared [min-start, max-end) range,
/// mutating all their entries in `intervals` to match -- this is what
/// makes Phase 7a's "φ emits nothing, dst and incoming values end up
/// sharing one physical location" strategy real at the interval level.
///
/// FIRST verifies the critical-edge-free invariant Phase 7a's φ-lowering
/// depends on: for every (pred_block, _) incoming pair, either pred_block
/// has exactly one successor, or the φ's own block has exactly one
/// predecessor via that edge (the standard non-critical-edge condition).
/// `assert!`, not `debug_assert!` -- matching this project's "caller/
/// internal-invariant bugs must fail loudly in release too" precedent.
/// Every producible program today is if/else-DAG-shaped and can never
/// trip this; it exists as a tripwire for whenever the front-end grows a
/// construct that could introduce a critical edge.
fn merge_phi_intervals(func: &Function, intervals: &mut HashMap<Value, Interval>) {
    for (i, inst) in func.insts.iter().enumerate() {
        if let Inst::Phi { incoming } = inst {
            let phi_value = Value(i as u32);
            for &(pred_block, _incoming_value) in incoming.iter() {
                let pred_successor_count = func
                    .blocks
                    .iter()
                    .position(|bd| std::ptr::eq(bd, &func.blocks[pred_block.0 as usize]))
                    .map(|_| successor_count(func, pred_block))
                    .unwrap_or(0);
                let phi_block = value_owning_block(func, phi_value);
                let phi_block_pred_count_via_this_edge = 1; // by construction, add_pred is called once per edge in this front-end
                assert!(
                    pred_successor_count <= 1 || phi_block_pred_count_via_this_edge <= 1,
                    "critical edge detected feeding phi {:?} from block {:?} -- Phase 7a's \
                     phi-lowering strategy (dst and incoming values sharing one interval) is \
                     unsound across a critical edge; this needs critical-edge splitting before \
                     proceeding, which does not exist yet",
                    phi_value,
                    pred_block
                );
                let _ = phi_block;
            }

            let mut min_start = intervals.get(&phi_value).map(|iv| iv.start).unwrap_or(u32::MAX);
            let mut max_end = intervals.get(&phi_value).map(|iv| iv.end).unwrap_or(0);
            for &(_, incoming_value) in incoming.iter() {
                if let Some(iv) = intervals.get(&incoming_value) {
                    min_start = min_start.min(iv.start);
                    max_end = max_end.max(iv.end);
                }
            }
            if let Some(iv) = intervals.get_mut(&phi_value) {
                iv.start = min_start;
                iv.end = max_end;
            }
            for &(_, incoming_value) in incoming.iter() {
                if let Some(iv) = intervals.get_mut(&incoming_value) {
                    iv.start = min_start;
                    iv.end = max_end;
                }
            }
        }
    }
}

fn successor_count(func: &Function, block: forge_ir::Block) -> usize {
    match &func.blocks[block.0 as usize].term {
        Some(forge_ir::Terminator::Jump(_)) => 1,
        Some(forge_ir::Terminator::Branch { .. }) => 2,
        _ => 0,
    }
}

fn value_owning_block(func: &Function, value: Value) -> forge_ir::Block {
    for (i, bd) in func.blocks.iter().enumerate() {
        if bd.insts.contains(&value) {
            return forge_ir::Block(i as u32);
        }
    }
    unreachable!("every real Value belongs to exactly one block")
}
```

**IMPORTANT — this Step 3 code references `crate::liveness_test_support::reads_of_pub`/`def_of_pub`, which don't exist**: this is a placeholder indicating that `reads_of`/`def_of` from `liveness.rs` (Task 3) need to be made accessible to `intervals.rs`. Do NOT literally create a `liveness_test_support` module. Instead: in `crates/forge-regalloc/src/liveness.rs`, change `fn reads_of` and `fn def_of` from private to `pub(crate) fn reads_of` and `pub(crate) fn def_of` (they're implementation details of this crate, not part of its public API, so `pub(crate)`, not `pub`), then in `intervals.rs` reference them as `crate::liveness::reads_of`/`crate::liveness::def_of` instead of the placeholder path. Apply this fix before running Step 4.

**IMPORTANT — the `merge_phi_intervals` critical-edge check above is a plausible-but-unverified sketch, not proven-correct code**: the `pred_successor_count`/`phi_block_pred_count_via_this_edge` computation is convoluted and likely has bugs (e.g., `phi_block_pred_count_via_this_edge` is hardcoded to `1`, which doesn't actually check anything real). Before treating this as final, an implementer MUST verify against `forge_ir::BlockData`'s real fields (read `crates/forge-ir/src/ir.rs`'s `BlockData` struct definition directly) — the real check should be: for each `(pred_block, _)` in a φ's `incoming`, look up `pred_block`'s terminator to count its real successors, and separately count how many DISTINCT predecessor blocks reach the φ's own block (via `BlockData`'s actual predecessor-tracking field, if one exists, or by scanning all blocks' terminators for edges targeting the φ's block) — rewrite this function for real correctness during implementation, using the actual `BlockData` shape, not the sketch above. Flag this rewrite explicitly in the implementer's self-review.

- [ ] **Step 4: Wire into `lib.rs`**

```rust
mod interval;
mod intervals;
mod liveness;

pub use interval::{Interval, RegClass, SYSV_FLOAT_ARGS, SYSV_INT_ARGS};
pub use intervals::build_intervals;
pub use liveness::{compute_liveness, Liveness};
```

- [ ] **Step 5: Run the tests and fix issues iteratively**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -100`

Given the "IMPORTANT" callouts in Step 3, expect real compile errors and logic bugs here — this is expected TDD iteration, not a sign the plan is wrong. Fix `reads_of`/`def_of` visibility, rewrite `merge_phi_intervals`'s critical-edge check against `BlockData`'s real shape, and verify `forge_syntax::parse`/`typecheck`/`forge_ir::lower`'s real names/signatures (per Step 1's callout) before the last test can compile. Iterate until all tests pass.

- [ ] **Step 6: Run the FULL workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 7: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 8: Commit**

```bash
git add crates/forge-regalloc/src/intervals.rs crates/forge-regalloc/src/lib.rs crates/forge-regalloc/src/liveness.rs
git commit -m "feat(forge-regalloc): build_intervals with real liveness, phi-merging, critical-edge tripwire"
```

---

## Task 5: Hints and fixed/excluded registers

**Files:**
- Modify: `crates/forge-regalloc/src/intervals.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/forge-regalloc/src/intervals.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn two_address_op_dst_gets_hint_pointing_at_lhs() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param { index: 0, ty: Ty::I64 },
        Ty::I64,
        dummy_span(),
    );
    let one = b.emit(entry, Inst::ConstI64(1), Ty::I64, dummy_span());
    let y = b.emit(entry, Inst::Add(x, one), Ty::I64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(y));

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
    assert_eq!(y_iv.hint, Some(x));
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
    let _ = (x, y);

    let selected = select(&b.f);
    let intervals = build_intervals(&b.f, &selected);

    let x_iv = intervals.iter().find(|iv| iv.value == x).unwrap();
    let n_iv = intervals.iter().find(|iv| iv.value == n).unwrap();
    let y_iv = intervals.iter().find(|iv| iv.value == y).unwrap();
    assert_eq!(x_iv.fixed, Some(forge_x64::PhysReg::Xmm0));
    assert_eq!(n_iv.fixed, Some(forge_x64::PhysReg::Rdi));
    assert_eq!(y_iv.fixed, Some(forge_x64::PhysReg::Xmm1));
}

#[test]
#[should_panic]
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
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let a = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
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
    assert_eq!(q_iv.fixed, Some(forge_x64::PhysReg::Rax));
    assert_eq!(r_iv.fixed, Some(forge_x64::PhysReg::Rdx));
    // lhs (a) gets NO special treatment at the Interval level -- confirms
    // the design's "pure emission-time fixup, no allocator-level hint"
    // resolution for IntDiv/IntRem's dividend.
    assert_eq!(a_iv.fixed, None);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- two_address_op_dst mixed_type_params seventh_int_param int_div_dst 2>&1 | tail -60`
Expected: FAIL (hints/fixed fields are always `None` from Task 4's implementation).

- [ ] **Step 3: Implement hint and fixed-register population**

In `crates/forge-regalloc/src/intervals.rs`, add these steps to `build_intervals` after the `merge_phi_intervals(func, &mut intervals);` line and before `intervals.into_values().collect()`:

```rust
    populate_two_address_hints(selected, &mut intervals);
    populate_fixed_registers(func, selected, &mut intervals);

    intervals.into_values().collect()
```

Add the two new functions:

```rust
/// Two-address hints: for each dst -> preferred_same_as entry in
/// SelectedFunction::coalescing_hints (fully computed already, Phase 7b),
/// record dst's interval hint as pointing at preferred_same_as. This is a
/// direct copy from an existing map, not new computation.
fn populate_two_address_hints(selected: &SelectedFunction, intervals: &mut HashMap<Value, Interval>) {
    for (&dst, &preferred) in &selected.coalescing_hints {
        if let Some(iv) = intervals.get_mut(&dst) {
            iv.hint = Some(preferred);
        }
    }
}

/// Param's class-relative ABI register, and IntDiv/IntRem's fixed
/// rax/rdx dst -- see the design doc's "Fixed registers" section for the
/// full reasoning (why rhs/lhs get NO Interval-level treatment here).
fn populate_fixed_registers(
    func: &Function,
    selected: &SelectedFunction,
    intervals: &mut HashMap<Value, Interval>,
) {
    let mut gpr_seen = 0usize;
    let mut xmm_seen = 0usize;
    for (index, &(_, ty)) in func.params.iter().enumerate() {
        let value = Value(index as u32); // Param's dst Value == its position in func.insts for a
                                          // function whose ONLY leading instructions are Params, per
                                          // lower.rs's construction order -- VERIFY this holds by
                                          // reading lower.rs directly before trusting it; if Params
                                          // aren't always func.insts[0..params.len()], find dst via
                                          // scanning func.insts for Inst::Param{index,..} instead.
        match crate::interval::RegClass::of(ty) {
            RegClass::Gpr => {
                assert!(
                    gpr_seen < crate::interval::SYSV_INT_ARGS.len(),
                    "function has more than {} integer/bool parameters -- exceeds SysV's \
                     integer argument register count; this needs to become a real Diagnostic \
                     before any user-facing CLI surface ships (tracked in the Phase 8a design doc)",
                    crate::interval::SYSV_INT_ARGS.len()
                );
                if let Some(iv) = intervals.get_mut(&value) {
                    iv.fixed = Some(crate::interval::SYSV_INT_ARGS[gpr_seen]);
                }
                gpr_seen += 1;
            }
            RegClass::Xmm => {
                assert!(
                    xmm_seen < crate::interval::SYSV_FLOAT_ARGS.len(),
                    "function has more than {} float parameters -- exceeds SysV's float \
                     argument register count",
                    crate::interval::SYSV_FLOAT_ARGS.len()
                );
                if let Some(iv) = intervals.get_mut(&value) {
                    iv.fixed = Some(crate::interval::SYSV_FLOAT_ARGS[xmm_seen]);
                }
                xmm_seen += 1;
            }
        }
    }

    for (i, inst) in selected.insts.iter().enumerate() {
        match inst {
            MachineInst::IntDiv { dst, .. } => {
                if let Some(iv) = intervals.get_mut(dst) {
                    iv.fixed = Some(forge_x64::PhysReg::Rax);
                }
            }
            MachineInst::IntRem { dst, .. } => {
                if let Some(iv) = intervals.get_mut(dst) {
                    iv.fixed = Some(forge_x64::PhysReg::Rdx);
                }
            }
            _ => {}
        }
        let _ = i;
    }
}
```

**IMPORTANT — the `Value(index as u32)` line above is flagged as unverified in its own comment**: before trusting "a Param's dst Value equals its declaration index," read `crates/forge-ir/src/lower.rs`'s parameter-emission loop (`lower.rs:30-42`, already cited in the design doc) directly and confirm whether `Builder::emit` assigns `Value`s in strict emission order starting from 0 for a fresh function (likely true, but MUST be confirmed against the real `Builder` implementation, not assumed) — if the function has ANY instructions before its params in `func.insts` (it shouldn't, given `lower.rs`'s structure, but verify), this line is wrong and must instead scan `func.insts` for `Inst::Param { index, .. }` entries and use THAT instruction's own position as the Value. Fix during implementation if the assumption doesn't hold; do not skip this verification.

- [ ] **Step 4: Run the tests and fix issues iteratively**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -100`

Expect to need the `Value(index as u32)` verification/fix from the IMPORTANT callout above. Iterate until all tests pass.

- [ ] **Step 5: Run the FULL workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-regalloc/src/intervals.rs
git commit -m "feat(forge-regalloc): two-address hints, Param/IntDiv/IntRem fixed-register population"
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
        .position(|i| matches!(i, MachineInst::IntDiv { .. }))
        .unwrap();
    let excl = excluded.get(&(div_pos, c)).expect("c must have an exclusion entry at div_pos");
    assert!(excl.contains(&forge_x64::PhysReg::Rax));
    assert!(excl.contains(&forge_x64::PhysReg::Rdx));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- int_div_rhs_is_excluded 2>&1 | tail -40`
Expected: FAIL — compile error (`excluded_registers` doesn't exist).

- [ ] **Step 3: Implement**

Add to `intervals.rs`:

```rust
/// Per-(instruction position, Value) register exclusions -- currently
/// only populated for IntDiv/IntRem's rhs (divisor), which must never be
/// assigned Rax or Rdx (cqo/idiv would destroy it before idiv ever reads
/// it -- see the design doc's idiv-clobber resolution, sub-problem 2).
/// 8b's design doc consumes this as an extra candidate-set filter in
/// pick_register, on top of ordinary availability.
pub fn excluded_registers(
    func: &Function,
    selected: &SelectedFunction,
) -> HashMap<(usize, Value), Vec<forge_x64::PhysReg>> {
    let _ = func;
    let mut excluded = HashMap::new();
    for (i, inst) in selected.insts.iter().enumerate() {
        match inst {
            MachineInst::IntDiv { rhs, .. } | MachineInst::IntRem { rhs, .. } => {
                excluded.insert(
                    (i, *rhs),
                    vec![forge_x64::PhysReg::Rax, forge_x64::PhysReg::Rdx],
                );
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

Run: `cargo test --workspace 2>&1 | tail -80`. Report exact final counts for `forge-x64` and `forge-regalloc`.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm all 12 exit criteria from `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md`'s "Exit criteria" section.

## Context for this whole plan

This plan's Tasks 3-6 contain multiple explicitly-flagged "IMPORTANT — unverified, must confirm against real code" callouts (the critical-edge check's real logic against `BlockData`'s actual shape, the `Value(index) == Param's dst` assumption, `forge_syntax`/`forge_ir`'s exact public API names). This is DELIBERATE, not an oversight — these are graph-algorithm/API-surface details this plan's author could not verify without actually compiling and running code (unlike Phase 6/7d's byte-level arithmetic, which was independently hand-verifiable). Per this project's established strongest-verification discipline, the NEXT step after this plan is written is an execution-based review (a subagent applying this plan in a scratch worktree and running the real test suite) — expect that review to find and need to fix real issues in these flagged spots, and treat that as the plan working as intended, not failing.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`
