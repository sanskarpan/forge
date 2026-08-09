# forge Phase 7b Two-Address Coalescing Hints & Lea Synthesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add coalescing-hint generation (`SelectedFunction::coalescing_hints`) and `lea`-synthesis fusion (`MachineInst::Lea`, recognizing both `Mul`-by-power-of-2 and its strength-reduced `Shl` form) to `forge-x64`'s instruction selector, extending Phase 7a's `machine_inst.rs`.

**Architecture:** Both pieces extend `Selector`/`SelectedFunction`/`select_inst` in the existing `crates/forge-x64/src/machine_inst.rs` — no new files. Coalescing hints are a post-pass over the finished `Vec<MachineInst>`. `lea` synthesis is a pre-pass (`find_fully_fusable_scaled_indices`, determining which `Mul`/`Shl`-defined values are fully subsumed by fusion) plus extensions to three existing `select_inst` arms (`Add` gains fusion, `Mul` and `Shl` both gain suppression). The pre-pass and the `Add` arm's fusion decision share one shape-matcher (`match_scaled_index`/`find_fusable_add`) so they can never disagree.

**Tech Stack:** Rust. No new dependencies.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-7b-two-address-hints-lea-synthesis-design.md` — read this first. This design went through four rounds of review (two real bugs found and fixed: a dead-code redundancy the pre-pass didn't originally prevent, and a key-on-wrong-Value bug that silently defeated suppression entirely) and one round of empirical execution verification (65/65 checks passed) — trust its code blocks as correct; they've been run, not just reasoned about.

**A note on running test counts:** this plan extends Phase 7a's `machine_inst` test module (55 tests as of Phase 7a's own final count, though always confirm via `cargo test -p forge-x64 --lib` rather than trusting any plan's arithmetic, per the established practice in every prior phase of this project).

---

## Task 1: `SelectedFunction::coalescing_hints` and `compute_coalescing_hints`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    #[test]
    fn coalescing_hints_binary_op_hints_dst_to_lhs_not_rhs() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Sub(x, y), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.coalescing_hints.get(&r), Some(&x));
        assert_ne!(selected.coalescing_hints.get(&r), Some(&y));
    }

    #[test]
    fn coalescing_hints_unary_op_hints_dst_to_src() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Neg(x), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.coalescing_hints.get(&r), Some(&x));
    }

    #[test]
    fn coalescing_hints_exclude_int_div_and_rem() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let d = b.emit(entry, Inst::Div(x, y), Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Rem(x, y), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.coalescing_hints.get(&d), None);
        assert_eq!(selected.coalescing_hints.get(&r), None);
    }

    #[test]
    fn coalescing_hints_no_entry_for_ops_with_no_natural_hint() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let p = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(p));

        let selected = select(&b.f);

        assert_eq!(selected.coalescing_hints.get(&p), None);
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -40`
Expected: FAIL — `SelectedFunction` has no `coalescing_hints` field yet (compile error).

- [ ] **Step 3: Add the field and the pass**

```rust
// crates/forge-x64/src/machine_inst.rs — add coalescing_hints to SelectedFunction

pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    /// dst -> the Value dst should end up sharing a physical register/slot
    /// with, if Phase 8's allocator can manage it. Every entry corresponds
    /// to a 2-address-destructive x86 operation where honoring the hint
    /// lets the final MachineInst-to-bytes emission step skip an
    /// otherwise-mandatory `mov dst, lhs` copy. A hint that isn't honored
    /// is not an error -- emission falls back to inserting the copy.
    pub coalescing_hints: HashMap<Value, Value>,
}
```

```rust
// crates/forge-x64/src/machine_inst.rs — add near select_inst/select_term,
// outside the Selector impl block (a free function, like select() itself)

/// Scans a fully-selected instruction sequence and records a dst->operand
/// coalescing hint for every 2-address-destructive MachineInst. Binary ops
/// hint dst->lhs (the operand whose register `dst` needs to already hold);
/// unary ops hint dst->src. IntDiv/IntRem are deliberately excluded -- their
/// constraint is fixed RAX/RDX placement, a different (fixed-register, not
/// coalescing) hint Phase 8's allocator handles separately. Lea is
/// deliberately excluded too -- real x86 lea is non-destructive 3-operand,
/// so it has no two-address constraint to hint around at all.
pub fn compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value> {
    let mut hints = HashMap::new();
    for inst in insts {
        match inst {
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
            MachineInst::IntNeg { dst, src }
            | MachineInst::Not { dst, src }
            | MachineInst::FloatNeg { dst, src, .. }
            | MachineInst::FloatAbs { dst, src, .. } => {
                hints.insert(*dst, *src);
            }
            _ => {}
        }
    }
    hints
}
```

```rust
// crates/forge-x64/src/machine_inst.rs — extend select()'s return to populate it

pub fn select(func: &Function) -> SelectedFunction {
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
    };
    for block in forge_ir::dominance::reverse_postorder(func) {
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
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: all 4 new tests pass, all Phase 7a tests still pass.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): SelectedFunction::coalescing_hints + compute_coalescing_hints"
```

## Context for this task

This task is independent of Task 2 (`lea` synthesis) — it doesn't touch `Add`/`Mul`/`Shl`'s dispatch logic at all, only adds a post-pass reading the already-finished `Vec<MachineInst>`. Do it first since it's the simpler of the two and has no dependency on Task 2's `Lea` variant existing (though `compute_coalescing_hints`'s match will need a `Lea` arm added in Task 2 — or, if `Lea` doesn't exist yet when this task runs, Rust's exhaustiveness won't force one since the match already has a `_ => {}` catch-all; Task 2 doesn't need to revisit this file's `compute_coalescing_hints` match at all, since `Lea` correctly falls into the existing wildcard).

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: Shared shape-matcher (`match_scaled_index`, `find_fusable_add`) and `MachineInst::Lea`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    #[test]
    fn lea_synthesis_mul_shape_operand_order_a() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[selected.insts.len() - 2],
            MachineInst::Lea { dst: add, base: base_v, index: idx, scale: 4, disp: 0 }
        );
        // No standalone IntMul for `mul` -- it was fully subsumed by fusion.
        assert!(!selected.insts.iter().any(
            |i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)
        ));
    }

    #[test]
    fn lea_synthesis_mul_shape_operand_order_b() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(base_v, mul), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[selected.insts.len() - 2],
            MachineInst::Lea { dst: add, base: base_v, index: idx, scale: 4, disp: 0 }
        );
        assert!(!selected.insts.iter().any(
            |i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)
        ));
    }

    #[test]
    fn lea_synthesis_shl_shape() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let two = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
        let shl = b.emit(entry, Inst::Shl(idx, two), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(shl, base_v), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[selected.insts.len() - 2],
            MachineInst::Lea { dst: add, base: base_v, index: idx, scale: 4, disp: 0 }
        );
        assert!(!selected.insts.iter().any(
            |i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl)
        ));
    }

    #[test]
    fn lea_synthesis_rejects_non_pow2_multiplier() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let three = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, three), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert!(!selected.insts.iter().any(|i| matches!(i, MachineInst::Lea { .. })));
        assert_eq!(selected.insts[3], MachineInst::IntMul { dst: mul, lhs: idx, rhs: three });
        assert_eq!(selected.insts[4], MachineInst::IntAdd { dst: add, lhs: mul, rhs: base_v });
    }

    #[test]
    fn lea_synthesis_rejects_non_constant_multiplier() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let other = b.emit(entry, Inst::Param { index: 2, ty: Ty::I64 }, Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, other), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert!(!selected.insts.iter().any(|i| matches!(i, MachineInst::Lea { .. })));
    }

    #[test]
    fn lea_synthesis_rejects_out_of_range_shift() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four_shift = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let shl = b.emit(entry, Inst::Shl(idx, four_shift), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(shl, base_v), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert!(!selected.insts.iter().any(|i| matches!(i, MachineInst::Lea { .. })));
        assert_eq!(selected.insts[3], MachineInst::Shl { dst: shl, lhs: idx, rhs: four_shift });
    }

    #[test]
    fn lea_synthesis_shared_consumer_both_fuse_and_suppress() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let idx = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let c1 = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let c2 = b.emit(entry, Inst::Param { index: 2, ty: Ty::I64 }, Ty::I64, dummy_span());
        let two = b.emit(entry, Inst::ConstI64(2), Ty::I64, dummy_span());
        let shl = b.emit(entry, Inst::Shl(idx, two), Ty::I64, dummy_span());
        let add1 = b.emit(entry, Inst::Add(shl, c1), Ty::I64, dummy_span());
        let add2 = b.emit(entry, Inst::Add(shl, c2), Ty::I64, dummy_span());
        let sum = b.emit(entry, Inst::Add(add1, add2), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

        let selected = select(&b.f);

        let leas: Vec<_> = selected
            .insts
            .iter()
            .filter(|i| matches!(i, MachineInst::Lea { .. }))
            .collect();
        assert_eq!(leas.len(), 2);
        assert!(!selected.insts.iter().any(
            |i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl)
        ));
    }

    #[test]
    fn lea_synthesis_escaping_use_prevents_suppression() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let base_v = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let idx = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
        let _add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
        // `mul` is ALSO directly returned -- an escaping use, so it must
        // NOT be suppressed even though the Add fuses it.
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(mul));

        let selected = select(&b.f);

        assert!(selected.insts.iter().any(|i| matches!(i, MachineInst::Lea { .. })));
        assert!(selected.insts.iter().any(
            |i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)
        ));
    }

    #[test]
    fn lea_synthesis_mixed_shape_both_operands_fusable_prefers_a() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let three_shift = b.emit(entry, Inst::ConstI64(3), Ty::I64, dummy_span());
        let mul_x = b.emit(entry, Inst::Mul(x, four), Ty::I64, dummy_span());
        let shl_y = b.emit(entry, Inst::Shl(y, three_shift), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(mul_x, shl_y), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        // mul_x (the `a` operand) is preferred as the fused index; shl_y
        // (the `b` operand) remains an ordinary Value used as `base`, and
        // gets its own independent, non-suppressed computation.
        assert_eq!(
            selected.insts[selected.insts.len() - 2],
            MachineInst::Lea { dst: add, base: shl_y, index: x, scale: 4, disp: 0 }
        );
        assert!(selected.insts.iter().any(
            |i| matches!(i, MachineInst::Shl { dst, .. } if *dst == shl_y)
        ));
        assert!(!selected.insts.iter().any(
            |i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul_x)
        ));
    }

    #[test]
    fn lea_synthesis_self_referential_add_never_suppresses() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let idx = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
        let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
        let add = b.emit(entry, Inst::Add(mul, mul), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(add));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[selected.insts.len() - 2],
            MachineInst::Lea { dst: add, base: mul, index: idx, scale: 4, disp: 0 }
        );
        // mul's own register genuinely still needs to exist (it's the
        // Lea's `base` operand too) -- must NOT be suppressed.
        assert!(selected.insts.iter().any(
            |i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)
        ));
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — `MachineInst::Lea` doesn't exist yet (compile error).

- [ ] **Step 3: Add `MachineInst::Lea`, the shared matcher, the suppression pre-pass, and wire it all into `select_inst`/`select()`**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the MachineInst enum,
// near the integer-arithmetic group

    Lea { dst: Value, base: Value, index: Value, scale: u8, disp: i32 },
```

```rust
// crates/forge-x64/src/machine_inst.rs — add as free functions, near select_inst/select_term

/// Free function (not a Selector method) so it has a single call site
/// usable both by the whole-function suppression pre-pass (which runs
/// BEFORE any Selector exists) and by Selector's Add arm during the main
/// walk -- the suppression decision and the fusion decision MUST agree
/// about which operand (if any) is "the fused one," so there is exactly
/// one implementation of this shape check, not two that could drift out
/// of sync.
///
/// Checks whether `candidate` is a real IR value (an index into
/// func.insts -- synthetic values are never Mul/Shl-defined and always
/// return None here) defined by a "scaled index" shape: `Mul(index,
/// ConstI64(k))`/`Mul(ConstI64(k), index)` for k in {2,4,8}, OR
/// `Shl(index, ConstI64(s))` for s in {1,2,3} (equivalent to k = 2^s --
/// strength-reduction rewrites the former into the latter for realistic
/// optimized input, so both are live, real shapes on different execution
/// tiers). If matched, returns (base, index, scale) with `base` set to
/// the OTHER argument passed in.
fn match_scaled_index(func: &Function, candidate: Value, other: Value) -> Option<(Value, Value, u8)> {
    if (candidate.0 as usize) >= func.insts.len() {
        return None;
    }
    let const_scale = |v: Value| -> Option<u8> {
        if (v.0 as usize) >= func.insts.len() {
            return None;
        }
        match &func.insts[v.0 as usize] {
            Inst::ConstI64(k) if matches!(k, 2 | 4 | 8) => Some(*k as u8),
            _ => None,
        }
    };
    match &func.insts[candidate.0 as usize] {
        Inst::Mul(m_a, m_b) => {
            if let Some(k) = const_scale(*m_b) {
                return Some((other, *m_a, k));
            }
            if let Some(k) = const_scale(*m_a) {
                return Some((other, *m_b, k));
            }
            None
        }
        Inst::Shl(index, shift_amount) => {
            if (shift_amount.0 as usize) >= func.insts.len() {
                return None;
            }
            match &func.insts[shift_amount.0 as usize] {
                Inst::ConstI64(s) if matches!(s, 1..=3) => Some((other, *index, 1u8 << s)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Tries both operand orderings of an Add(a, b) for the scaled-index
/// shape, preferring `a` as the fused operand if both individually
/// qualify (Add(Mul(x,4), Shl(y,3)) picks x/4 as index, leaving y's Shl
/// as an ordinary Value feeding the Lea's `base` -- NOT itself
/// fused/suppressed).
fn find_fusable_add(func: &Function, a: Value, b: Value) -> Option<(Value, Value, u8)> {
    match_scaled_index(func, a, b).or_else(|| match_scaled_index(func, b, a))
}

/// Run once, before the main RPO walk, over the WHOLE function. Determines
/// which real IR Values, if any, are Mul/Shl results fully subsumed by lea
/// fusion (every use is a fusable Add pattern, none escape to any other
/// consumer) and therefore safe to suppress the same way Phi is.
fn find_fully_fusable_scaled_indices(func: &Function) -> std::collections::HashSet<Value> {
    let mut total_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        for used in forge_ir::uses_of(inst) {
            *total_uses.entry(used).or_insert(0) += 1;
        }
    }
    // uses_of only covers Inst, never Terminator -- a directly-returned or
    // branched-on Value must still count as used, or it would be wrongly
    // suppressed even though the terminator needs its real computed value.
    for block in &func.blocks {
        match &block.term {
            Some(Terminator::Return(v)) => *total_uses.entry(*v).or_insert(0) += 1,
            Some(Terminator::Branch { cond, .. }) => *total_uses.entry(*cond).or_insert(0) += 1,
            _ => {}
        }
    }

    // NOTE: this must key on the OUTER Add's own operand (`a` or `b` --
    // one of which IS the Mul/Shl's defining Value) that matched, NOT on
    // match_scaled_index's returned `index` (which is the raw scaled
    // register INSIDE the matched Mul/Shl -- a completely different
    // Value). Keying on the wrong Value here silently defeats suppression
    // entirely.
    let mut fusable_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        if let Inst::Add(a, b) = inst {
            if match_scaled_index(func, *a, *b).is_some() {
                *fusable_uses.entry(*a).or_insert(0) += 1;
            } else if match_scaled_index(func, *b, *a).is_some() {
                *fusable_uses.entry(*b).or_insert(0) += 1;
            }
        }
    }

    total_uses
        .into_iter()
        .filter(|(v, total)| fusable_uses.get(v).copied().unwrap_or(0) == *total)
        .map(|(v, _)| v)
        .collect()
}
```

```rust
// crates/forge-x64/src/machine_inst.rs — Selector gains one new field

struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
    fully_fusable_scaled_indices: std::collections::HashSet<Value>,
}
```

```rust
// crates/forge-x64/src/machine_inst.rs — select()'s setup, extended:
// find_fully_fusable_scaled_indices(func) MUST run BEFORE the Selector is
// constructed. (Task 1's coalescing_hints wiring at the end is unchanged.)

pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
    };
    for block in forge_ir::dominance::reverse_postorder(func) {
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
    }
}
```

```rust
// crates/forge-x64/src/machine_inst.rs — select_inst's Inst::Add arm,
// replacing the existing I64 branch:

            Inst::Add(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
                Ty::I64 => match find_fusable_add(self.func, *a, *b) {
                    Some((base, index, scale)) => {
                        self.insts.push(MachineInst::Lea { dst, base, index, scale, disp: 0 })
                    }
                    None => self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b }),
                },
                Ty::Bool => unreachable!("Add never applies to Bool"),
            },
```

```rust
// crates/forge-x64/src/machine_inst.rs — select_inst's EXISTING Inst::Mul
// arm's I64 branch, replacing the unconditional push:

            Inst::Mul(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatMul { dst, lhs: *a, rhs: *b }),
                Ty::I64 => {
                    if !self.fully_fusable_scaled_indices.contains(&dst) {
                        self.insts.push(MachineInst::IntMul { dst, lhs: *a, rhs: *b });
                    }
                    // else: fully subsumed by lea fusion, nothing to emit --
                    // same suppression discipline as Inst::Phi.
                }
                Ty::Bool => unreachable!("Mul never applies to Bool"),
            },
```

```rust
// crates/forge-x64/src/machine_inst.rs — select_inst's EXISTING Inst::Shl
// arm, replacing the unconditional push:

            Inst::Shl(a, b) => {
                if !self.fully_fusable_scaled_indices.contains(&dst) {
                    self.insts.push(MachineInst::Shl { dst, lhs: *a, rhs: *b });
                }
                // else: fully subsumed by lea fusion, nothing to emit.
            }
```

`HashMap` is already imported at the top of the file (`use std::collections::HashMap;`) and used unqualified throughout, matching the code above. `HashSet` is used fully-qualified (`std::collections::HashSet`) everywhere it appears (the `Selector` field, `find_fully_fusable_scaled_indices`'s return type) — no new `use` statement is needed; this is a deliberate, self-consistent choice, not an open decision.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -40`
Expected: all 10 new tests pass, all Phase 7a tests and Task 1's 4 new tests still pass.

- [ ] **Step 5: Run the FULL workspace test suite to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): lea synthesis for Add(Mul-or-Shl-by-pow2, c), with full DAG-aware suppression"
```

## Context for this task

This is the highest-risk task in this plan — the design it implements went through four review rounds (two real bugs found), and this task's tests are specifically the ones that would have caught both bugs (the single-consumer suppression assertions, the shared-consumer test, the escaping-use test). Every code block above has already been executed and verified correct in a standalone harness during design review — transcribe it faithfully rather than "improving" it, since subtle rewording of the suppression logic (e.g. keying on the wrong `Value`) is exactly what went wrong twice already.

`compute_coalescing_hints` (Task 1) does NOT need any changes for `Lea` — it already falls into that match's `_ => {}` catch-all, correctly producing no hint (real `lea` is non-destructive).

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -60`. Report exact final counts.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm all 7 exit criteria from the design doc are met:
1. `SelectedFunction::coalescing_hints` exists, populated by `compute_coalescing_hints`, covering every 2-address-destructive `MachineInst` variant, correctly excluding `IntDiv`/`IntRem`, and correctly excluding `Lea`.
2. `MachineInst::Lea` exists; `select_inst`'s `Inst::Add`/`I64` case recognizes both `Mul`- and `Shl`-shaped scaled indices via the shared helpers; falls back to plain `IntAdd` otherwise.
3. `find_fully_fusable_scaled_indices` correctly suppresses a fused `Mul`'s or `Shl`'s standalone `MachineInst` exactly when every use was absorbed by fusion, and never otherwise. Both the `Mul` and `Shl` arms carry the suppression check.
4. Tests cover both operand orderings for both shapes, all negative cases, the shared-consumer case, the escaping-use case, the mixed-shape preference-order case, and the self-referential case.
5. `cargo test --workspace` green, clippy/fmt clean.
6. No regressions in any Phase 6 `forge-x64` test, Phase 7a's `machine_inst` tests, or any other crate's tests.
7. CHECKLIST.md annotated — this happens as part of the separate final holistic review dispatched after this plan's Task 3 completes, not as a task within this plan document.
