# Phase 7f — Select→cmov Diamond Fusion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement empty-arm diamond fusion in `crates/forge-x64`'s `select()` — recognizing `if cond then x else y`-shaped diamonds (where `x`/`y` are already-live values, no computation in either arm) and rewriting them to branchless `MachineInst`s: `FloatMin`/`FloatMax` for the float min/max pattern, a new `MachineInst::IntCmov` for the general integer case.

**Architecture:** A new pre-pass `find_fusable_diamonds(func) -> (HashMap<Block, DiamondFusion>, HashSet<Block>)` in `crates/forge-x64/src/machine_inst/mod.rs`, run once before `Selector` construction (mirroring `find_fully_fusable_scaled_indices`'s exact template from Phase 7b). `select()`'s RPO walk skips arm blocks in the returned set and, when it reaches a fusion-keyed `Branch` terminator, emits the fused `MachineInst` instead of `Branch`+two-block-walk. A new `MachineInst::IntCmov` variant requires explicit arms in every exhaustive `MachineInst` match across the workspace (`reads_of`/`def_of` in `forge-regalloc`, `compute_coalescing_hints` in `forge-x64` itself).

**Tech Stack:** Rust, `crates/forge-x64` (primary), `crates/forge-regalloc` (exhaustive-match fallout only, no new logic).

**Design doc:** `docs/superpowers/specs/2026-08-10-phase-7f-select-cmov-diamond-fusion-design.md` — execution-verified through two full review rounds, both building and running real code against this codebase's actual types. The first round found and the second round confirmed the fix for two real, source-reachable miscompiles (a backwards float min/max operand-order table; a missing type gate on the general cmov path that let float diamonds produce a bogus `IntCmov` over XMM-classed values) plus a falsified "exactly one Phi at the merge block" claim and a silent `compute_coalescing_hints` gap. Treat every corrected rule, table row, and code shape in the design doc as verified — but this plan's own code blocks (written directly from that design, not independently re-executed) have NOT yet been through their own execution-based review; that is this plan's own next step after self-review, per this project's established two-review-round cadence (design review, then plan review, before implementation).

---

## Before you start

Read `crates/forge-x64/src/machine_inst/mod.rs` in full (`select()`, `select_term`, `select_inst`'s `Inst::Phi` arm, `find_fully_fusable_scaled_indices`, `compute_coalescing_hints`, the full `MachineInst` enum) and `crates/forge-ir/src/ir.rs` (`Inst`, `Terminator`, `BlockData`, `CmpOp`, `Ty`). Confirm baseline: `cargo test --workspace` currently green (Phase 8e's shipped state, 448+ tests across the workspace).

**The one architectural fact every task below must respect**: `crates/forge-x64` does NOT depend on `crates/forge-regalloc` (dependency runs the other way). `forge-regalloc::RegClass` is NOT nameable from `forge-x64`. Wherever this plan needs "is this value GPR-equivalent," it checks `ty_of(v) != Ty::F64` directly (`forge_ir::Ty` has exactly `F64`/`I64`/`Bool`; `F64` is the only XMM-classed type anywhere in this codebase), never `RegClass::of`.

---

### Task 1: `find_fusable_diamonds` — the detection pre-pass

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`

- [ ] **Step 1: Write the failing tests**

**On test construction strategy**: `forge_ir::builder::Builder`'s phi-insertion (`new_phi`/`fill_phi_operands`) is PRIVATE and driven entirely by its `write_variable`/`read_variable`-based Braun-style SSA construction algorithm — there is no public API to hand-construct an arbitrary phi shape directly through `Builder`. Confirmed by reading `crates/forge-ir/src/builder.rs` directly (not assumed): `Function`, `BlockData`, `Inst`, and `Terminator` all have fully `pub` fields/variants (`Function { insts: Vec<Inst>, types: Vec<Ty>, spans: Vec<Span>, blocks: Vec<BlockData>, entry: Block, params }`, `BlockData { insts: Vec<Value>, term: Option<Terminator>, preds: SmallVec<[Block; 2]> } ` derives `Default`), so these tests construct `Function`s DIRECTLY via struct literals instead of going through `Builder` at all — reliable, since it only depends on public fields, and `find_fusable_diamonds` (Step 3 below) never trusts `BlockData::preds` anyway (predecessors are re-derived from real terminators), so leaving `preds` at its `Default` empty value in these hand-built fixtures is correct, not an oversight.

Add near `find_fully_fusable_scaled_indices`'s own test module (or a new `#[cfg(test)] mod diamond_tests` if that's cleaner given the existing file's organization — match whichever the file's current structure suggests):

```rust
#[cfg(test)]
mod diamond_fusion_tests {
    use super::*;
    use forge_ir::{Block, BlockData, CmpOp, Function, Inst, Terminator, Ty, Value};

    /// Mirrors Builder::emit's exact logic (push to insts/types/spans, push
    /// the index into the target block's insts) without going through
    /// Builder itself, since this file only needs the plain data shape.
    fn push_inst(func: &mut Function, block: Block, inst: Inst, ty: Ty) -> Value {
        let v = Value(func.insts.len() as u32);
        func.insts.push(inst);
        func.types.push(ty);
        func.spans.push(forge_syntax::span::Span::new(0, 0));
        func.blocks[block.0 as usize].insts.push(v);
        v
    }

    fn empty_func(num_blocks: usize) -> Function {
        Function {
            insts: Vec::new(),
            types: Vec::new(),
            spans: Vec::new(),
            blocks: vec![BlockData::default(); num_blocks],
            entry: Block(0),
            params: Vec::new(),
        }
    }

    /// Builds: entry(0) has Branch(cond, t=1, e=2); t and e are empty,
    /// both Jump to m=3; m has a Phi(t: val_t, e: val_e) plus optionally
    /// a trivial phi (same value on both edges), then Return(phi_dst).
    /// Returns (func, cond, val_t, val_e, phi_dst).
    fn build_diamond(
        val_ty: Ty,
        cmp: Option<CmpOp>,
        swap_incoming: bool,
        extra_trivial_phi: bool,
    ) -> (Function, Value, Value, Value, Value) {
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));

        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: val_ty }, val_ty);
        let c = push_inst(&mut func, entry, Inst::Param { index: 1, ty: val_ty }, val_ty);
        let cond = if let Some(op) = cmp {
            push_inst(&mut func, entry, Inst::Cmp { op, lhs: a, rhs: c }, Ty::Bool)
        } else {
            push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool)
        };
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));

        let (val_t, val_e) = if swap_incoming { (c, a) } else { (a, c) };
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, val_t), (e, val_e)] },
            val_ty,
        );
        if extra_trivial_phi {
            push_inst(
                &mut func,
                m,
                Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, a)] },
                val_ty,
            );
        }
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        (func, cond, val_t, val_e, phi_dst)
    }

    #[test]
    fn eligible_diamond_is_detected_as_int_cmov() {
        let (func, cond, val_t, val_e, phi_dst) = build_diamond(Ty::I64, None, false, false);
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::IntCmov { dst, cond: c, then_val, else_val } => {
                assert_eq!(*dst, phi_dst);
                assert_eq!(*c, cond);
                assert_eq!(*then_val, val_t);
                assert_eq!(*else_val, val_e);
            }
            other => panic!("expected IntCmov, got {other:?}"),
        }
        assert_eq!(skip.len(), 2);
    }

    #[test]
    fn non_empty_arm_is_rejected() {
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64);
        let c = push_inst(&mut func, entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64);
        let cond = push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        // t computes a real division -- NOT empty. This is the
        // correctness-critical near-miss: fusing this would unconditionally
        // execute IntDiv, which traps on c==0 even when cond would have
        // avoided taking this arm.
        let divided = push_inst(&mut func, t, Inst::Div(a, c), Ty::I64);
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, divided), (e, c)] },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "a non-empty arm must never be fused -- IntDiv traps");
        assert!(skip.is_empty());
    }

    #[test]
    fn multiple_phis_at_merge_only_the_differing_one_is_the_payload() {
        let (func, _cond, _val_t, _val_e, phi_dst) =
            build_diamond(Ty::I64, None, false, true); // extra trivial phi
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1, "the trivial phi must not block fusion");
        match fusions.values().next().unwrap() {
            DiamondFusion::IntCmov { dst, .. } => assert_eq!(*dst, phi_dst),
            other => panic!("expected IntCmov, got {other:?}"),
        }
        assert_eq!(skip.len(), 2);
    }

    #[test]
    fn two_differing_phis_at_merge_is_rejected() {
        // Two independent values differ between the arms -- this design's
        // single-value fusion cannot represent that; must not fuse.
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64);
        let c = push_inst(&mut func, entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64);
        let cond = push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi1 = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, c)] },
            Ty::I64,
        );
        push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, c), (e, a)] },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi1));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty());
        assert!(skip.is_empty());
    }

    #[test]
    fn third_predecessor_of_merge_is_rejected() {
        let mut func = empty_func(5);
        let (entry, t, e, other, m) = (Block(0), Block(1), Block(2), Block(3), Block(4));
        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64);
        let c = push_inst(&mut func, entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64);
        let cond = push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        // `other` is an unrelated block that ALSO jumps to `m` -- a genuine
        // 3rd predecessor, unreachable from `entry` in this fixture (that's
        // fine; find_fusable_diamonds only counts real terminator edges
        // into `m`, it never requires the whole function be reachable).
        func.blocks[other.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, c)] },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "m has a 3rd predecessor (`other`) -- must not fuse");
        assert!(skip.is_empty());
    }

    #[test]
    fn float_min_max_table_row_1_lt_a_b_is_min() {
        let (func, ..) = build_diamond(Ty::F64, Some(CmpOp::Lt), false, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax { op: MinMaxOp::Min, .. } => {}
            other => panic!("expected FloatMinMax::Min, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_2_lt_b_a_is_max_with_swapped_operands() {
        let (func, _, val_t, val_e, _) = build_diamond(Ty::F64, Some(CmpOp::Lt), true, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        assert_eq!(fusions.len(), 1);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax { op: MinMaxOp::Max, lhs, rhs, .. } => {
                // The else-arm value (val_e) must be rhs; the then-arm
                // value (val_t) must be lhs -- this is the exact defect
                // execution-based design review found and fixed.
                assert_eq!(*lhs, val_t);
                assert_eq!(*rhs, val_e);
            }
            other => panic!("expected FloatMinMax::Max, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_3_gt_a_b_is_max() {
        let (func, ..) = build_diamond(Ty::F64, Some(CmpOp::Gt), false, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax { op: MinMaxOp::Max, .. } => {}
            other => panic!("expected FloatMinMax::Max, got {other:?}"),
        }
    }

    #[test]
    fn float_min_max_table_row_4_gt_b_a_is_min_with_swapped_operands() {
        let (func, _, val_t, val_e, _) = build_diamond(Ty::F64, Some(CmpOp::Gt), true, false);
        let (fusions, _) = find_fusable_diamonds(&func);
        match fusions.values().next().unwrap() {
            DiamondFusion::FloatMinMax { op: MinMaxOp::Min, lhs, rhs, .. } => {
                assert_eq!(*lhs, val_t);
                assert_eq!(*rhs, val_e);
            }
            other => panic!("expected FloatMinMax::Min, got {other:?}"),
        }
    }

    #[test]
    fn float_le_ge_diamonds_produce_no_fusion_at_all() {
        let (func_le, ..) = build_diamond(Ty::F64, Some(CmpOp::Le), false, false);
        let (fusions_le, skip_le) = find_fusable_diamonds(&func_le);
        assert!(fusions_le.is_empty(), "Le must not fuse -- no derived table for it");
        assert!(skip_le.is_empty());

        let (func_ge, ..) = build_diamond(Ty::F64, Some(CmpOp::Ge), false, false);
        let (fusions_ge, skip_ge) = find_fusable_diamonds(&func_ge);
        assert!(fusions_ge.is_empty(), "Ge must not fuse -- no derived table for it");
        assert!(skip_ge.is_empty());
    }

    #[test]
    fn float_third_value_diamond_produces_no_fusion_never_int_cmov() {
        // if a > b then a else c -- val_e (c) is NOT one of the comparison's
        // own operands, so this can't be a min/max rewrite, and it must NOT
        // fall through to IntCmov either (that path is Ty::I64/Bool only --
        // this is the exact miscompile execution-based design review found).
        let mut func = empty_func(4);
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64);
        let bb = push_inst(&mut func, entry, Inst::Param { index: 1, ty: Ty::F64 }, Ty::F64);
        let cc = push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::F64 }, Ty::F64);
        let cond = push_inst(&mut func, entry, Inst::Cmp { op: CmpOp::Gt, lhs: a, rhs: bb }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, cc)] },
            Ty::F64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "float third-value diamond must produce NO fusion, never IntCmov");
        assert!(skip.is_empty());
    }

    #[test]
    fn f64_diamond_with_a_non_cmp_cond_is_gated_off_the_int_cmov_path() {
        // Closes a real coverage gap execution-based plan review found by
        // mutation testing: float_third_value_diamond_produces_no_fusion
        // above never actually reaches the `ty_of(dst) != Ty::F64` hard
        // gate, because its `cond` is an Inst::Cmp over F64 operands,
        // which the EARLIER min/max-table branch already rejects (`continue`)
        // before the general IntCmov path is ever considered. Deleting the
        // hard gate entirely left every existing test passing -- this
        // fixture uses a non-Cmp cond (an ordinary bool value, matching
        // `cmp: None` in build_diamond) with F64-typed arm values, which
        // is the ONLY shape that actually exercises the gate: nothing
        // about `cond` routes it through the min/max branch at all, so
        // without the hard gate this would silently become an IntCmov
        // over Xmm-classed values -- the exact miscompile this design's
        // whole "general cmov path" section exists to prevent.
        let (func, ..) = build_diamond(Ty::F64, None, false, false);
        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "F64 dst must never become an IntCmov");
        assert!(skip.is_empty());
    }

    #[test]
    fn arm_with_an_extra_predecessor_is_rejected() {
        // Found by mutation testing during plan review: deleting the
        // `pred_count[t] == 1 && pred_count[e] == 1` guard left the whole
        // test suite green -- nothing else exercises "an arm block has a
        // second, unrelated predecessor." Without the guard, `t` still
        // looks empty (rule 2 only checks t/e's own insts), so `other`'s
        // real Jump(t) target silently vanishes once t is skipped.
        let mut func = empty_func(5);
        let (entry, t, e, m, other) = (Block(0), Block(1), Block(2), Block(3), Block(4));
        let a = push_inst(&mut func, entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64);
        let c = push_inst(&mut func, entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64);
        let cond = push_inst(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[other.0 as usize].term = Some(Terminator::Jump(t));
        let phi_dst = push_inst(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, c)] },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let (fusions, skip) = find_fusable_diamonds(&func);
        assert!(fusions.is_empty(), "arm `t` has a 2nd predecessor -- must not fuse");
        assert!(skip.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo build -p forge-x64 --tests 2>&1 | tail -30`
Expected: fails to compile — `find_fusable_diamonds`, `DiamondFusion`, `MinMaxOp` don't exist yet.

- [ ] **Step 3: Implement `DiamondFusion`, `MinMaxOp`, and `find_fusable_diamonds`**

Add to `crates/forge-x64/src/machine_inst/mod.rs`, near `find_fully_fusable_scaled_indices`:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MinMaxOp {
    Min,
    Max,
}

/// A diamond eligible for branchless fusion (see design doc). Keyed by
/// the diamond's BRANCH block in the map `find_fusable_diamonds` returns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DiamondFusion {
    FloatMinMax {
        dst: Value,
        op: MinMaxOp,
        lhs: Value,
        rhs: Value,
    },
    IntCmov {
        dst: Value,
        cond: Value,
        then_val: Value,
        else_val: Value,
    },
}

/// Run once, before the main RPO walk, over the WHOLE function -- exact
/// architectural mirror of `find_fully_fusable_scaled_indices` above.
/// Detects every diamond eligible for fusion: a Branch block whose two
/// arms are BOTH EMPTY (BlockData::insts.is_empty()) and both Jump to
/// the SAME merge block, which has no other predecessor, and which has
/// exactly one Phi whose two incoming values genuinely DIFFER (any other
/// Phis at the merge block must be trivial -- same value on both edges
/// -- or the diamond isn't fused; a real corpus function can have
/// several phis at one merge block, only one of which is this diamond's
/// own payload). See design doc for the full correctness reasoning
/// (why empty arms specifically, why this can't just use `RegClass`).
pub fn find_fusable_diamonds(
    func: &Function,
) -> (HashMap<Block, DiamondFusion>, std::collections::HashSet<Block>) {
    let mut fusions = HashMap::new();
    let mut skip = std::collections::HashSet::new();

    // Predecessor counts re-derived from real terminators, never trusted
    // from BlockData::preds (which is Builder's own bookkeeping and can
    // be stale on a hand-built Function) -- same discipline already
    // established in forge-regalloc/src/intervals.rs's critical-edge check.
    let mut pred_count: HashMap<Block, u32> = HashMap::new();
    for block in &func.blocks {
        match &block.term {
            Some(Terminator::Jump(b)) => *pred_count.entry(*b).or_insert(0) += 1,
            Some(Terminator::Branch { then_, else_, .. }) => {
                *pred_count.entry(*then_).or_insert(0) += 1;
                if then_ != else_ {
                    *pred_count.entry(*else_).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }

    let ty_of = |v: Value| -> Ty {
        if (v.0 as usize) < func.types.len() {
            func.types[v.0 as usize]
        } else {
            // No synthetic values exist at the IR level (only select()
            // mints those) -- func.types must always cover a real IR Value.
            unreachable!("Value {v:?} has no IR-level type")
        }
    };

    for (idx, block_data) in func.blocks.iter().enumerate() {
        let pred = Block(idx as u32);
        let Some(Terminator::Branch { cond, then_: t, else_: e }) = &block_data.term else {
            continue;
        };
        if t == e {
            continue;
        }
        let t_data = &func.blocks[t.0 as usize];
        let e_data = &func.blocks[e.0 as usize];
        if !t_data.insts.is_empty() || !e_data.insts.is_empty() {
            continue;
        }
        let (Some(Terminator::Jump(mt)), Some(Terminator::Jump(me))) = (&t_data.term, &e_data.term)
        else {
            continue;
        };
        if mt != me {
            continue;
        }
        let m = *mt;
        if pred_count.get(&m).copied().unwrap_or(0) != 2 {
            continue;
        }
        // Both arms must ALSO have exactly one predecessor each (the
        // Branch block itself) -- found by mutation testing during plan
        // review: without this check, a third, unrelated block that also
        // jumps INTO an otherwise-eligible arm block still gets skipped by
        // select() (both arms are still "empty," matching rule 2), so that
        // third block's real Jump target vanishes with nothing to redirect
        // it -- unreachable from this front-end's structured if/else
        // lowering today, but not something find_fusable_diamonds's other
        // checks happen to rule out on their own, so it's checked directly.
        if pred_count.get(t).copied().unwrap_or(0) != 1
            || pred_count.get(e).copied().unwrap_or(0) != 1
        {
            continue;
        }

        let m_data = &func.blocks[m.0 as usize];
        let mut payload: Option<(Value, Value, Value)> = None; // (phi_dst, val_t, val_e)
        let mut two_differing = false;
        for &v in &m_data.insts {
            if let Inst::Phi { incoming } = &func.insts[v.0 as usize] {
                if incoming.len() != 2 {
                    continue;
                }
                let (b0, v0) = incoming[0];
                let (b1, v1) = incoming[1];
                let (val_t, val_e) = if b0 == *t && b1 == *e {
                    (v0, v1)
                } else if b0 == *e && b1 == *t {
                    (v1, v0)
                } else {
                    continue; // this phi's edges don't match t/e at all -- ignore
                };
                if val_t != val_e {
                    if payload.is_some() {
                        two_differing = true;
                    }
                    payload = Some((v, val_t, val_e));
                }
            }
        }
        if two_differing {
            continue;
        }
        let Some((dst, val_t, val_e)) = payload else {
            continue; // no differing phi at all -- nothing this diamond fuses
        };

        // Float min/max table (see design doc for the corrected 4-row
        // derivation -- else-arm value is ALWAYS rhs, never determined by
        // the comparison operator alone).
        if let Inst::Cmp { op, lhs, rhs } = &func.insts[cond.0 as usize] {
            if ty_of(*lhs) == Ty::F64 {
                let rewrite = match op {
                    CmpOp::Lt if val_t == *lhs && val_e == *rhs => {
                        Some(DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Min, lhs: *lhs, rhs: *rhs })
                    }
                    CmpOp::Lt if val_t == *rhs && val_e == *lhs => {
                        Some(DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Max, lhs: *rhs, rhs: *lhs })
                    }
                    CmpOp::Gt if val_t == *lhs && val_e == *rhs => {
                        Some(DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Max, lhs: *lhs, rhs: *rhs })
                    }
                    CmpOp::Gt if val_t == *rhs && val_e == *lhs => {
                        Some(DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Min, lhs: *rhs, rhs: *lhs })
                    }
                    _ => None,
                };
                if let Some(fusion) = rewrite {
                    fusions.insert(pred, fusion);
                    skip.insert(*t);
                    skip.insert(*e);
                }
                // F64 comparison but not the min/max shape (Le/Ge, a
                // third unrelated value, or Eq/Ne) -- NEVER falls through
                // to IntCmov (that path is Ty::I64/Bool only). Simply
                // unfused, whether or not `rewrite` matched above.
                continue;
            }
        }

        // General integer path -- hard type gate, not a fallback. `dst`'s
        // type (not `cond`'s) determines whether IntCmov is legal, since
        // `dst` is what the cmov actually writes.
        if ty_of(dst) != Ty::F64 {
            fusions.insert(
                pred,
                DiamondFusion::IntCmov { dst, cond: *cond, then_val: val_t, else_val: val_e },
            );
            skip.insert(*t);
            skip.insert(*e);
        }
    }

    (fusions, skip)
}
```

- [ ] **Step 4: Run to verify all tests pass**

Run: `cargo test -p forge-x64 diamond_fusion_tests`
Expected: PASS (13 tests). Two `FloatMinMax` pattern matches (rows 2 and 4) must destructure with a trailing `, ..` — `DiamondFusion::FloatMinMax` has a 4th field (`dst`) those two tests don't otherwise reference, and Rust's non-exhaustive-pattern check (`E0027`) will reject the match without it.

- [ ] **Step 5: Commit**

```bash
cd /Users/sanskar/dev/Research/Projects/JIT-Compiler
git add crates/forge-x64/src/machine_inst/mod.rs
git commit -m "feat(forge-x64): add find_fusable_diamonds diamond-detection pre-pass"
```

---

### Task 2: `MachineInst::IntCmov` and the exhaustive-match fallout

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-regalloc/src/liveness.rs`

- [ ] **Step 1: Add the `IntCmov` variant**

Add to `MachineInst`'s enum definition in `crates/forge-x64/src/machine_inst/mod.rs` (place it near the other Int* variants for organizational consistency):

```rust
    /// Branchless select: dst = (cond != 0) ? then_val : else_val. Produced
    /// ONLY by diamond fusion (find_fusable_diamonds) -- select_inst never
    /// constructs this directly, since there is no Inst::Select to match
    /// on (see the design doc). 2-address destructive like every other
    /// x86 binary op: dst starts as a copy of then_val (a coalescing hint
    /// records dst -> then_val -- see compute_coalescing_hints), and
    /// emission (deferred, task #68) overwrites it with else_val
    /// conditionally via `test cond, cond` (reads cond's already-
    /// materialized 0/1 value directly, NOT the flags from whatever
    /// produced it) followed by `cmovz dst, else_val`. Requires cond to
    /// be genuinely zero-extended to 64 bits by whatever materialized it
    /// (setcc alone only writes 1 byte) -- a real precondition on task
    /// #68's emission of IntCmp/FloatCmp, not something this variant or
    /// its selection-time construction can itself guarantee.
    IntCmov {
        dst: Value,
        cond: Value,
        then_val: Value,
        else_val: Value,
    },
```

- [ ] **Step 2: Run to find every exhaustive match that now fails to compile**

Run: `cargo build --workspace --all-targets 2>&1 | grep "error\[E0004\]" -A 3`
Expected: at least the 2 sites the design doc names (`reads_of`/`def_of` in `crates/forge-regalloc/src/liveness.rs`). Note every file/line reported — there may be others this plan didn't anticipate; if so, that's real information the design's blast-radius estimate was tested against, not a plan defect (fix each one following the same pattern as the others).

- [ ] **Step 3: Add `IntCmov` arms to `reads_of`/`def_of`**

In `crates/forge-regalloc/src/liveness.rs`, find `reads_of`'s match and add (grouped with other 2-operand-plus-extra-operand instructions, or as its own arm — match the file's existing style):

```rust
        MachineInst::IntCmov { cond, then_val, else_val, .. } => {
            vec![*cond, *then_val, *else_val]
        }
```

Find `def_of`'s match and add:

```rust
        MachineInst::IntCmov { dst, .. } => Some(*dst),
```

(Match these to the exact existing pattern style in the file — e.g. if `def_of` uses a big `|`-chained pattern with a shared body, `IntCmov { dst, .. }` likely joins that chain rather than getting a fully separate arm; read the surrounding code and follow its convention rather than introducing a new style.)

- [ ] **Step 4: Add the `compute_coalescing_hints` arm — the silent gap that would NOT be caught by the compiler**

In `crates/forge-x64/src/machine_inst/mod.rs`'s `compute_coalescing_hints`, conceptually `IntCmov` belongs with the FIRST match arm group (the `hints.insert(*dst, *lhs)` group `IntAdd`/`IntSub`/etc. already belong to — same "hint dst toward the operand it destructively reuses" idea, `then_val` playing `lhs`'s role). It cannot actually JOIN that `|`-chain, though: the chain requires every variant to bind the exact same field name (`lhs`), and `IntCmov`'s corresponding field is named `then_val`. Add it as its own separate arm instead, immediately after that chain closes:

```rust
            MachineInst::IntAdd { dst, lhs, .. }
            | MachineInst::IntSub { dst, lhs, .. }
            | MachineInst::IntMul { dst, lhs, .. }
            | MachineInst::And { dst, lhs, .. }
            | MachineInst::Or { dst, lhs, .. }
            | MachineInst::Xor { dst, lhs, .. }
            | MachineInst::Shl { dst, lhs, .. }
            | MachineInst::Shr { dst, lhs, .. }
            | MachineInst::Sar { dst, lhs, .. }
            | MachineInst::FloatAdd { dst, lhs, .. }
            | MachineInst::FloatSub { dst, lhs, .. }
            | MachineInst::FloatMul { dst, lhs, .. }
            | MachineInst::FloatDiv { dst, lhs, .. }
            | MachineInst::FloatMin { dst, lhs, .. }
            | MachineInst::FloatMax { dst, lhs, .. } => {
                hints.insert(*dst, *lhs);
            }
```

The new arm:

```rust
            MachineInst::IntCmov { dst, then_val, .. } => {
                hints.insert(*dst, *then_val);
            }
```

- [ ] **Step 5: Write a test confirming the coalescing hint is real**

Add to `crates/forge-x64/src/machine_inst/mod.rs`'s existing test module (wherever `compute_coalescing_hints` is already tested, following that file's pattern):

```rust
    #[test]
    fn int_cmov_gets_a_coalescing_hint_to_then_val() {
        let insts = vec![MachineInst::IntCmov {
            dst: Value(3),
            cond: Value(2),
            then_val: Value(0),
            else_val: Value(1),
        }];
        let hints = compute_coalescing_hints(&insts);
        assert_eq!(hints.get(&Value(3)), Some(&Value(0)));
    }
```

- [ ] **Step 6: Run the full workspace build and test suite**

Run: `cargo build --workspace --all-targets 2>&1 | tail -30`
Expected: clean, no remaining `E0004` errors.

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs crates/forge-regalloc/src/liveness.rs
git commit -m "feat(forge-x64): add MachineInst::IntCmov and its exhaustive-match arms"
```

---

### Task 3: Wire fusion into `select()`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`

- [ ] **Step 1: Write the failing integration test**

Add to `crates/forge-x64/src/machine_inst/mod.rs`'s test module:

```rust
    #[test]
    fn select_fuses_an_eligible_diamond_and_skips_arm_blocks() {
        // Same direct-Function-construction style as
        // diamond_fusion_tests::eligible_diamond_is_detected_as_int_cmov
        // (Builder's phi insertion is private/algorithm-driven -- see that
        // test module's own note), but runs the REAL select() end to end.
        use forge_ir::{Block, BlockData, Function, Inst, Terminator, Ty, Value};

        let mut func = Function {
            insts: Vec::new(),
            types: Vec::new(),
            spans: Vec::new(),
            blocks: vec![BlockData::default(); 4],
            entry: Block(0),
            params: Vec::new(),
        };
        let (entry, t, e, m) = (Block(0), Block(1), Block(2), Block(3));
        let push = |func: &mut Function, block: Block, inst: Inst, ty: Ty| -> Value {
            let v = Value(func.insts.len() as u32);
            func.insts.push(inst);
            func.types.push(ty);
            func.spans.push(forge_syntax::span::Span::new(0, 0));
            func.blocks[block.0 as usize].insts.push(v);
            v
        };
        let a = push(&mut func, entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64);
        let c = push(&mut func, entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64);
        let cond = push(&mut func, entry, Inst::Param { index: 2, ty: Ty::Bool }, Ty::Bool);
        func.blocks[entry.0 as usize].term = Some(Terminator::Branch { cond, then_: t, else_: e });
        func.blocks[t.0 as usize].term = Some(Terminator::Jump(m));
        func.blocks[e.0 as usize].term = Some(Terminator::Jump(m));
        let phi_dst = push(
            &mut func,
            m,
            Inst::Phi { incoming: smallvec::smallvec![(t, a), (e, c)] },
            Ty::I64,
        );
        func.blocks[m.0 as usize].term = Some(Terminator::Return(phi_dst));

        let selected = select(&func);

        // Exactly one IntCmov, no MachineInst::Branch, no MachineInst::Jump
        // for the fused diamond's arms.
        let cmovs: Vec<_> = selected.insts.iter().filter(|i| matches!(i, MachineInst::IntCmov { .. })).collect();
        assert_eq!(cmovs.len(), 1);
        assert!(!selected.insts.iter().any(|i| matches!(i, MachineInst::Branch { .. })));

        // block_starts still has an entry for every block, including the
        // two now-empty arms (zero-length ranges).
        assert_eq!(selected.block_starts.len(), 4);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p forge-x64 select_fuses_an_eligible_diamond`
Expected: FAIL — `select()` doesn't consult `find_fusable_diamonds` yet, so this produces `MachineInst::Branch` + `Jump`s instead.

- [ ] **Step 3: Wire the pre-pass into `Selector` and `select()`**

Change `Selector`'s struct definition to add the fusion map:

```rust
struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
    fully_fusable_scaled_indices: std::collections::HashSet<Value>,
    pool: ConstantPool,
    diamond_fusions: HashMap<Block, DiamondFusion>,
}
```

Change `select()`:

```rust
pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let (diamond_fusions, diamond_skip_blocks) = find_fusable_diamonds(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
        pool: ConstantPool::default(),
        diamond_fusions,
    };
    let mut block_starts = Vec::new();
    for block in forge_ir::dominance::reverse_postorder(func) {
        block_starts.push((block, sel.insts.len()));
        if diamond_skip_blocks.contains(&block) {
            // An empty diamond arm -- contributes NOTHING to insts, same
            // "two adjacent block_starts entries share one start" shape
            // as a block that selects to zero MachineInsts elsewhere in
            // this file (Phi, fully-suppressed Mul/Shl).
            continue;
        }
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(fusion) = sel.diamond_fusions.get(&block).copied() {
            // This block's real Branch terminator is replaced by the
            // fused instruction -- no Jump/Branch emitted for it at all.
            // The fused instruction falls through directly into the
            // merge block, which RPO visits immediately next since it's
            // this block's sole remaining real successor once both arms
            // are elided.
            match fusion {
                DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Min, lhs, rhs } => {
                    sel.insts.push(MachineInst::FloatMin { dst, lhs, rhs });
                }
                DiamondFusion::FloatMinMax { dst, op: MinMaxOp::Max, lhs, rhs } => {
                    sel.insts.push(MachineInst::FloatMax { dst, lhs, rhs });
                }
                DiamondFusion::IntCmov { dst, cond, then_val, else_val } => {
                    sel.insts.push(MachineInst::IntCmov { dst, cond, then_val, else_val });
                }
            }
            // The fused instruction falls through directly into the merge
            // block `m` -- re-derived here from `t`'s own terminator (`t`
            // is always Jump(m) by find_fusable_diamonds's own eligibility
            // rules), NOT assumed. This debug_assert checks m's IDENTITY,
            // not merely "something comes next" -- a check against only
            // "is there a next block at all" would be trivially true for
            // any fused diamond (m always exists and is never itself a
            // skip block, since it has real computation: the payload phi
            // is what made this a fusion in the first place), so it must
            // name m explicitly to mean anything.
            let Terminator::Branch { then_: t, .. } = func.blocks[block.0 as usize].term.as_ref().unwrap() else {
                unreachable!("a fusion key's block must have a Branch terminator")
            };
            let Some(Terminator::Jump(m)) = &func.blocks[t.0 as usize].term else {
                unreachable!("find_fusable_diamonds guarantees t's terminator is Jump(m)")
            };
            debug_assert!(
                {
                    let rpo = forge_ir::dominance::reverse_postorder(func);
                    let this_pos = rpo.iter().position(|b| *b == block).unwrap();
                    let next_real = rpo[this_pos + 1..]
                        .iter()
                        .find(|b| !diamond_skip_blocks.contains(b));
                    // Confirmed true by execution across every real corpus
                    // program and every hand-built fixture during plan
                    // review, but this front-end's structured-CFG guarantee
                    // is not something find_fusable_diamonds itself
                    // enforces, so a violation must fail loudly here rather
                    // than silently fall through into the wrong block with
                    // no Jump to redirect it.
                    next_real == Some(m)
                },
                "diamond fusion's merge block was not the next RPO-visited block"
            );
        } else if let Some(term) = &func.blocks[block.0 as usize].term {
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

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p forge-x64 select_fuses_an_eligible_diamond`
Expected: PASS.

- [ ] **Step 5: Run the full `forge-x64` test suite**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all green — confirm nothing about `select()`'s existing, non-diamond behavior regressed (every prior Phase 7a-7e test still passes unchanged, since `find_fusable_diamonds` returns empty maps for every program with no eligible diamond).

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs
git commit -m "feat(forge-x64): wire diamond fusion into select()'s RPO walk"
```

---

### Task 4: Corpus-wide regression, CHECKLIST annotation, final verification

**Files:**
- Create: `crates/forge-x64/tests/diamond_fusion_corpus.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/Cargo.toml`
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Write the corpus-wide `verify_allocation` regression test — as an EXTERNAL integration test, not a `src/` unit test**

**This MUST be an integration test in `crates/forge-x64/tests/`, not a unit test inside `src/machine_inst/mod.rs` — confirmed by execution during plan review, not a hypothetical concern.** A `forge-x64` DEV-dependency on `forge-regalloc` (needed since `forge-regalloc` depends on `forge-x64` in the real build graph) makes Cargo build TWO separate instances of `forge-x64`: the `--cfg test` lib-test instance (where a `src/`-internal `#[test]` would run) and the plain lib instance that `forge-regalloc` itself links against. These are DISTINCT crate instances to the type system — `SelectedFunction` constructed by the in-crate `select()` and the `SelectedFunction` `forge_regalloc::build_intervals` expects would be two different types, producing a real `E0308` mismatch, not a hypothetical one. Integration tests in `tests/*.rs` don't have this problem — they link against the real, single lib instance, the same one `forge-regalloc` sees.

Add `forge-regalloc = { path = "../forge-regalloc" }` to `crates/forge-x64/Cargo.toml`'s `[dev-dependencies]` (this dev-dependency cycle is fine for an integration test specifically, per the reasoning above).

Add `find_fusable_diamonds`, `DiamondFusion`, `MinMaxOp` to `crates/forge-x64/src/lib.rs`'s existing `pub use machine_inst::{...}` line (currently `select, ConstantPool, MachineInst, PoolIndex, SelectedFunction`) — the integration test needs these as public API, since `mod machine_inst;` itself is private.

Create `crates/forge-x64/tests/diamond_fusion_corpus.rs`:

```rust
#[test]
fn fused_output_across_the_whole_corpus_still_produces_valid_allocations() {
    let corpus = [
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
        "if x > y then (x * y) + (x - y) else x / y",
        "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
    ];
    let mut fused_any = 0;
    for src in corpus {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        let func = forge_ir::lower::lower(&typed);

        let (fusions, _) = forge_x64::find_fusable_diamonds(&func);
        if !fusions.is_empty() {
            fused_any += 1;
        }

        let selected = forge_x64::select(&func);
        let intervals = forge_regalloc::build_intervals(&func, &selected);
        let excluded = forge_regalloc::excluded_registers(&func, &selected);
        let (assignment, _bytes) = forge_regalloc::allocate(intervals.clone(), &excluded, &selected);

        assert!(
            forge_regalloc::verify_allocation(&intervals, &assignment).is_ok(),
            "{src:?}: fused output must still produce a valid, independently-verified allocation"
        );
    }
    assert!(
        fused_any > 0,
        "corpus must contain at least one fusable diamond, or this test is vacuous -- \
         confirmed by design review that \"(if a > b then a else b) + a\" and the inner \
         diamond of \"if a > b then (if a > c then a else c) else b\" both fuse"
    );
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p forge-x64 --test diamond_fusion_corpus`
Expected: PASS, and confirm (e.g. via a temporary `eprintln!` you remove afterward, or by reasoning from the design doc's own execution-confirmed count) that `fused_any == 2` — the design doc's review confirmed exactly 2 of these 18 programs contain a fusable diamond. If your count differs, investigate why before proceeding (either a real behavior difference from what was verified, or a corpus/test transcription error).

- [ ] **Step 3: Run the full workspace verification**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: all green.

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -60`
Expected: clean.

Run: `cargo fmt --check`
Expected: clean. If it reports diffs, run `cargo fmt` and re-check.

- [ ] **Step 4: Annotate CHECKLIST.md**

Find the bullet: `Select → cmov (integer) or vblendvpd / minsd+maxsd idioms (float) — branchless where profitable`. Its existing note already says "explicitly deferred to Phase 7f" — extend it (do not replace the existing 7b note, APPEND a new `— **note (Phase 7f):** ...` after it) with what was actually built: empty-arm diamond fusion only (not general arm-computation fusion — a real, stated scope reduction), the corrected float min/max table (4 rows, strict `<`/`>` only, `Le`/`Ge` and float third-value diamonds unfused), the hard `Ty::I64`/`Ty::Bool`-only gate on the general `IntCmov` path, and — matching this project's established honesty convention — a note that no real profitability BENCHMARK exists yet (still blocked on task #68's not-yet-built emission pipeline), so "branchless where profitable" is satisfied by design-time reasoning (the empty-arm shape's branch-misprediction-elimination case), not by measurement. Point to `docs/superpowers/specs/2026-08-10-phase-7f-select-cmov-diamond-fusion-design.md`.

- [ ] **Step 5: Commit**

```bash
git add crates/forge-x64/tests/diamond_fusion_corpus.rs crates/forge-x64/src/lib.rs crates/forge-x64/Cargo.toml CHECKLIST.md
git commit -m "test(forge-x64): corpus-wide fusion regression + CHECKLIST annotation"
```

---

## Self-review notes (already applied above, recorded for the implementer's context)

- **Spec coverage**: every exit criterion from the design doc has a corresponding task/step above — the detection pre-pass (Task 1), the new variant and its exhaustive-match/coalescing-hint fallout (Task 2), the `select()` wiring including the `debug_assert!` and `block_starts` handling (Task 3), and the corpus regression plus CHECKLIST annotation (Task 4).
- **Type consistency check**: `find_fusable_diamonds(func: &Function) -> (HashMap<Block, DiamondFusion>, HashSet<Block>)`, `DiamondFusion`, `MinMaxOp`, and `MachineInst::IntCmov { dst, cond, then_val, else_val }` are used identically across every task and test above.
- **Placeholder scan**: no task above contains a TBD or an unshown code block. An earlier draft of Task 1 used `forge_ir::builder::Builder`'s `new_phi`/`fill_phi_operands` methods, guessed from research rather than read from the real file — checking `crates/forge-ir/src/builder.rs` directly found both are PRIVATE and driven by a `write_variable`/`read_variable`-based Braun-style algorithm with no public hand-construction API, so every hand-built test fixture in Task 1 and Task 3 was rewritten to construct `Function`/`BlockData` directly via their public fields instead (reliable, and consistent with how this codebase already hand-builds other structs — e.g. `Interval` literals throughout `forge-regalloc`'s tests — when a lower-level struct literal is simpler than going through a higher-level builder).

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-10-phase-7f-select-cmov-diamond-fusion-plan.md`. Per this project's established cadence, this plan is next sent to a dispatched subagent for its own execution-based review — actually building this plan's code in a scratch worktree and running everything — before subagent-driven implementation begins.
