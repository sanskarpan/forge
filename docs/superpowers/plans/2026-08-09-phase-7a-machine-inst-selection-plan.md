# forge Phase 7a MachineInst + Baseline Instruction Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `MachineInst` enum and a baseline tree-tiling selector (`select(&Function) -> SelectedFunction`) in `forge-x64`, lowering every `forge_ir::Inst` variant except `Call` (explicitly deferred to Phase 7e) into `MachineInst`s over virtual registers (`forge_ir::Value`, reused directly).

**Architecture:** New file `crates/forge-x64/src/machine_inst.rs`. `MachineInst` is a flat enum, one variant per real operation family, always in 3-address SSA form. Selection walks blocks in reverse postorder (`forge_ir::dominance::reverse_postorder`, already implemented) and lowers each block's instructions in order, minting fresh synthetic `Value`s (via a `next_value` counter seeded from `func.insts.len()`, collision-free since `Value` numbering is append-only across this codebase's whole optimizer pipeline) for the few cases (`Fma`, `Abs`, `Neg` on floats) needing an intermediate result. `Phi` emits nothing (deferred to Phase 8's SSA deconstruction); `Call` panics with a clear "ships in 7e" message via an exhaustive match.

**Tech Stack:** Rust. New dependency: `forge-x64` gains a dependency on `forge-ir` (its first — `forge-x64` has been encoder-only through Phase 6).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-7a-machine-inst-selection-design.md` — read this first, especially its "Context: resolving two real ambiguities" section.

**A note on running test counts:** this is a new file/module, not an extension of `assembler.rs`/`round_trip.rs` — there is no prior running count to extend. Each task states its own new test count; trust `cargo test -p forge-x64` over any arithmetic in this plan if they ever diverge.

**A note on `Inst` variant shapes:** `forge_ir::Inst`'s arithmetic/bitwise/shift/unary variants are **tuple** variants (`Add(Value, Value)`, `Neg(Value)`, etc.), not struct variants — only `Param`, `Fma`, `Cmp`, `Call`, `Phi` are struct variants (`{ field: ... }`). Match arms below reflect this exactly; double-check this distinction if a match arm fails to compile.

---

## Task 1: `MachineInst` enum, `SelectedFunction`, and the `select()` skeleton (constants, params, terminators)

**Files:**
- Modify: `crates/forge-x64/Cargo.toml`
- Create: `crates/forge-x64/src/machine_inst.rs`
- Modify: `crates/forge-x64/src/lib.rs`

- [ ] **Step 1: Add the `forge-ir` dependency**

```toml
# crates/forge-x64/Cargo.toml — full file contents

[package]
name = "forge-x64"
version.workspace = true
edition.workspace = true

[dependencies]
forge-ir = { path = "../forge-ir" }

[dev-dependencies]
iced-x86.workspace = true
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — append at the end of the file, after the code from Step 4

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::{Builder, Inst, Terminator, Ty};
    use forge_syntax::span::Span;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn select_lowers_a_single_i64_constant_and_return() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let c = b.emit(entry, Inst::ConstI64(42), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts,
            vec![
                MachineInst::LoadImmI64 { dst: c, imm: 42 },
                MachineInst::Return { value: c },
            ]
        );
        assert!(selected.synthetic_types.is_empty());
    }

    #[test]
    fn select_lowers_an_f64_constant() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let bits = 3.5f64.to_bits();
        let c = b.emit(entry, Inst::ConstF64(bits), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts,
            vec![
                MachineInst::LoadImmF64 { dst: c, bits },
                MachineInst::Return { value: c },
            ]
        );
    }

    #[test]
    fn select_lowers_a_bool_constant_as_zero_or_one() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let t = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(t));

        let selected = select(&b.f);

        assert_eq!(selected.insts[0], MachineInst::LoadImmI64 { dst: t, imm: 1 });
    }

    #[test]
    fn select_lowers_a_param() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let p = b.emit(
            entry,
            Inst::Param { index: 0, ty: Ty::F64 },
            Ty::F64,
            dummy_span(),
        );
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(p));

        let selected = select(&b.f);

        assert_eq!(selected.insts[0], MachineInst::Param { dst: p, index: 0 });
    }

    /// Two blocks joined by an unconditional jump -- confirms RPO block
    /// ordering (entry's Jump comes before target's contents in the
    /// output, matching visitation order, not just definition order).
    #[test]
    fn select_lowers_jump_and_visits_blocks_in_rpo() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let target = b.create_block();
        b.add_pred(target, entry);
        b.seal_block(entry);
        b.seal_block(target);
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Jump(target));
        let c = b.emit(target, Inst::ConstI64(7), Ty::I64, dummy_span());
        b.f.blocks[target.0 as usize].term = Some(Terminator::Return(c));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts,
            vec![
                MachineInst::Jump { target },
                MachineInst::LoadImmI64 { dst: c, imm: 7 },
                MachineInst::Return { value: c },
            ]
        );
    }

    #[test]
    fn select_lowers_branch() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_b = b.create_block();
        let else_b = b.create_block();
        b.add_pred(then_b, entry);
        b.add_pred(else_b, entry);
        b.seal_block(entry);
        b.seal_block(then_b);
        b.seal_block(else_b);
        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_b,
            else_: else_b,
        });
        let t = b.emit(then_b, Inst::ConstI64(1), Ty::I64, dummy_span());
        b.f.blocks[then_b.0 as usize].term = Some(Terminator::Return(t));
        let e = b.emit(else_b, Inst::ConstI64(0), Ty::I64, dummy_span());
        b.f.blocks[else_b.0 as usize].term = Some(Terminator::Return(e));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[1],
            MachineInst::Branch { cond, then_: then_b, else_: else_b }
        );
    }
}
```

**IMPORTANT**: `forge_ir::Builder`/`Inst`/`Terminator`/`Ty` must be importable as shown — check `crates/forge-ir/src/lib.rs`'s re-exports (`pub use ir::*;` plus `pub mod builder;`) and adjust the `use` line if `Builder` isn't re-exported at the crate root (it may need `use forge_ir::builder::Builder;` instead of `use forge_ir::Builder;` — verify by checking whether `builder.rs`'s `Builder` is re-exported, and use whichever import path actually compiles).

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -40`
Expected: FAIL — `machine_inst` module/`select`/`MachineInst` not defined yet (compile error).

- [ ] **Step 4: Write the implementation**

```rust
// crates/forge-x64/src/machine_inst.rs — full file contents (tests from Step 2 go at the end, in the #[cfg(test)] mod already shown above)

use forge_ir::{Block, CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

/// A machine-level instruction operating on virtual registers (SSA Values,
/// reused directly from forge-ir -- one virtual register per SSA value, no
/// separate VReg type). Sits between forge-ir's `Inst` and forge-x64's
/// `Assembler` calls. Still in 3-address SSA form: `dst` is always a fresh
/// value distinct from its operands, even for opcodes that are 2-address-
/// destructive on real x86 (IntAdd/FloatAdd/And/etc). Two-address fixup
/// (Phase 7b) doesn't rewrite this form -- it only attaches coalescing
/// hints, consumed later by Phase 8's allocator and by the final
/// MachineInst-to-bytes emission step (built once Phase 8 exists), which
/// decides whether an actual copy is needed based on real register
/// assignments.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineInst {
    // Constants (ConstBool lowers through LoadImmI64 as 0/1)
    LoadImmI64 { dst: Value, imm: i64 },
    LoadImmF64 { dst: Value, bits: u64 },

    // Integer arithmetic -- destructive (dst must end up == lhs's location)
    IntAdd { dst: Value, lhs: Value, rhs: Value },
    IntSub { dst: Value, lhs: Value, rhs: Value },
    IntMul { dst: Value, lhs: Value, rhs: Value },
    IntDiv { dst: Value, lhs: Value, rhs: Value }, // cqo + idiv; RAX/RDX-fixed, Phase 8's concern
    IntRem { dst: Value, lhs: Value, rhs: Value }, // same shape, takes RDX instead of RAX
    IntNeg { dst: Value, src: Value },
    And { dst: Value, lhs: Value, rhs: Value },
    Or { dst: Value, lhs: Value, rhs: Value },
    Xor { dst: Value, lhs: Value, rhs: Value },
    Not { dst: Value, src: Value },
    Shl { dst: Value, lhs: Value, rhs: Value }, // rhs must end up in CL, Phase 8's concern
    Shr { dst: Value, lhs: Value, rhs: Value },
    Sar { dst: Value, lhs: Value, rhs: Value },

    // Float arithmetic -- destructive (dst must end up == lhs's location)
    FloatAdd { dst: Value, lhs: Value, rhs: Value },
    FloatSub { dst: Value, lhs: Value, rhs: Value },
    FloatMul { dst: Value, lhs: Value, rhs: Value },
    FloatDiv { dst: Value, lhs: Value, rhs: Value },
    FloatSqrt { dst: Value, src: Value },
    FloatMin { dst: Value, lhs: Value, rhs: Value },
    FloatMax { dst: Value, lhs: Value, rhs: Value },
    FloatRound { dst: Value, src: Value, mode: forge_x64_round_mode::RoundModeShim },

    // Abs/Neg on floats: mask_tmp is a synthetic I64 Value holding the
    // sign-mask constant, minted by the selector -- see machine_inst.rs's
    // Fma/Abs/Neg lowering for why this field exists (it lets the
    // post-Phase-8 emission step synthesize the exact movq+andpd/xorpd
    // sequence once mask_tmp's and dst's real registers are known).
    FloatAbs { dst: Value, src: Value, mask_tmp: Value },
    FloatNeg { dst: Value, src: Value, mask_tmp: Value },

    // Comparisons -- resolved to a specific strategy at selection time
    IntCmp { op: CmpOp, dst: Value, lhs: Value, rhs: Value }, // cmp + setcc, signed codes
    FloatCmp { op: CmpOp, dst: Value, lhs: Value, rhs: Value }, // ucomisd + setcc, UNSIGNED codes

    // Conversions
    IntToFloat { dst: Value, src: Value },
    FloatToInt { dst: Value, src: Value }, // truncating (cvttsd2si)

    // Control flow
    Jump { target: Block },
    Branch { cond: Value, then_: Block, else_: Block },
    Return { value: Value },

    // Parameters
    Param { dst: Value, index: u32 },
}

/// The result of instruction selection: a flat MachineInst sequence plus
/// the Ty of every virtual register the selector minted that ISN'T a real
/// IR value (i.e. every synthetic temp -- Fma's mul_tmp, Abs/Neg's
/// mask_tmp). Phase 8 needs this to know GPR-vs-XMM class for registers
/// `func.types` doesn't cover; real IR values look their Ty up in
/// `func.types` directly via this module's own `ty_of` helper.
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
}

struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
}

impl<'a> Selector<'a> {
    /// Looks up a Value's Ty whether it's a real IR value (func.types) or
    /// a synthetic temp this selector minted (synthetic_types). Value
    /// numbering is append-only across this codebase's whole optimizer
    /// pipeline (verified: no pass compacts or renumbers `f.insts`), so
    /// `next_value` seeded from `func.insts.len()` never collides with a
    /// real Value, and this dispatch on index is safe.
    fn ty_of(&self, v: Value) -> Ty {
        if (v.0 as usize) < self.func.types.len() {
            self.func.types[v.0 as usize]
        } else {
            self.synthetic_types[&v]
        }
    }

    fn fresh(&mut self, ty: Ty) -> Value {
        let v = Value(self.next_value);
        self.next_value += 1;
        self.synthetic_types.insert(v, ty);
        v
    }

    fn select_inst(&mut self, dst: Value, inst: &Inst) {
        match inst {
            Inst::ConstF64(bits) => self.insts.push(MachineInst::LoadImmF64 { dst, bits: *bits }),
            Inst::ConstI64(v) => self.insts.push(MachineInst::LoadImmI64 { dst, imm: *v }),
            Inst::ConstBool(v) => {
                self.insts.push(MachineInst::LoadImmI64 { dst, imm: *v as i64 })
            }
            Inst::Param { index, .. } => self.insts.push(MachineInst::Param { dst, index: *index }),

            // Remaining variants are filled in by later tasks in this plan.
            _ => todo!("filled in by Tasks 2-6 of the Phase 7a plan"),
        }
    }

    fn select_term(&mut self, term: &Terminator) {
        match term {
            Terminator::Return(v) => self.insts.push(MachineInst::Return { value: *v }),
            Terminator::Jump(b) => self.insts.push(MachineInst::Jump { target: *b }),
            Terminator::Branch { cond, then_, else_ } => self.insts.push(MachineInst::Branch {
                cond: *cond,
                then_: *then_,
                else_: *else_,
            }),
        }
    }
}

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
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
    }
}
```

**IMPORTANT — placeholder note**: the `FloatRound` variant's `mode` field type above (`forge_x64_round_mode::RoundModeShim`) is a deliberately obviously-wrong placeholder name — replace it with the crate's real `RoundMode` type (`crate::assembler::RoundMode`, re-exported as `crate::RoundMode` per `lib.rs`) before compiling. This was written this way so it's impossible to miss; do not leave it as `RoundModeShim`. The `select_inst` match's `_ => todo!(...)` arm makes this task's implementation intentionally incomplete — Rust will not warn about this since `todo!()` satisfies exhaustiveness, but every test in Step 2 above only exercises the variants already implemented (`ConstF64`/`ConstI64`/`ConstBool`/`Param` plus terminators), so they must all pass without hitting the `todo!()`.

- [ ] **Step 5: Fix the `RoundMode` type and wire the module into `lib.rs`**

```rust
// crates/forge-x64/src/machine_inst.rs — replace the FloatRound variant's mode field type

    FloatRound { dst: Value, src: Value, mode: crate::RoundMode },
```

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod machine_inst;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, RoundMode, ShiftOp, SseOp};
pub use machine_inst::{select, MachineInst, SelectedFunction};
pub use reg::PhysReg;
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: all 6 new tests pass (`select_lowers_a_single_i64_constant_and_return`, `select_lowers_an_f64_constant`, `select_lowers_a_bool_constant_as_zero_or_one`, `select_lowers_a_param`, `select_lowers_jump_and_visits_blocks_in_rpo`, `select_lowers_branch`).

- [ ] **Step 7: Run the FULL workspace test suite to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -60`
Expected: every existing test in every crate still passes — this task added a new dependency edge (`forge-x64` → `forge-ir`) and a new module, but touched no existing code.

- [ ] **Step 8: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 9: Commit**

```bash
git add crates/forge-x64/Cargo.toml crates/forge-x64/src/machine_inst.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): MachineInst enum, SelectedFunction, select() skeleton (constants/params/terminators)"
```

## Context for this task

This is the foundational task for the whole Phase 7a slice — every later task in this plan extends `select_inst`'s `match` (replacing pieces of the `_ => todo!(...)` catch-all with real arms) rather than restructuring anything here. `Selector::ty_of`/`fresh` are the two mechanisms every later task relies on: `ty_of` for dispatching int-vs-float lowering strategies, `fresh` for minting synthetic temporaries (`Fma`'s `mul_tmp`, `Abs`/`Neg`'s `mask_tmp`).

If the `use forge_ir::{Builder, ...}` import in the test module doesn't compile, check `crates/forge-ir/src/lib.rs`'s actual re-export list — `Builder` lives in `builder.rs`; confirm whether `lib.rs` re-exports it at the crate root or whether it needs `forge_ir::builder::Builder` instead, and use whichever form actually resolves.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: Integer arithmetic, bitwise, and shift lowering

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    /// Builds a block with two i64 params and one binary-op instruction
    /// between them, returning the op's result -- the shared shape every
    /// test below uses, parameterized by which Inst to build and which
    /// MachineInst it should lower to.
    fn select_i64_binop(inst_ctor: impl FnOnce(Value, Value) -> Inst) -> (SelectedFunction, Value, Value, Value) {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, inst_ctor(x, y), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
        let selected = select(&b.f);
        (selected, x, y, r)
    }

    #[test]
    fn select_lowers_int_add() {
        let (selected, x, y, r) = select_i64_binop(Inst::Add);
        assert_eq!(selected.insts[2], MachineInst::IntAdd { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_int_sub() {
        let (selected, x, y, r) = select_i64_binop(Inst::Sub);
        assert_eq!(selected.insts[2], MachineInst::IntSub { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_int_mul() {
        let (selected, x, y, r) = select_i64_binop(Inst::Mul);
        assert_eq!(selected.insts[2], MachineInst::IntMul { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_int_div() {
        let (selected, x, y, r) = select_i64_binop(Inst::Div);
        assert_eq!(selected.insts[2], MachineInst::IntDiv { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_int_rem() {
        let (selected, x, y, r) = select_i64_binop(Inst::Rem);
        assert_eq!(selected.insts[2], MachineInst::IntRem { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_and() {
        let (selected, x, y, r) = select_i64_binop(Inst::And);
        assert_eq!(selected.insts[2], MachineInst::And { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_or() {
        let (selected, x, y, r) = select_i64_binop(Inst::Or);
        assert_eq!(selected.insts[2], MachineInst::Or { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_xor() {
        let (selected, x, y, r) = select_i64_binop(Inst::Xor);
        assert_eq!(selected.insts[2], MachineInst::Xor { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_shl() {
        let (selected, x, y, r) = select_i64_binop(Inst::Shl);
        assert_eq!(selected.insts[2], MachineInst::Shl { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_shr() {
        let (selected, x, y, r) = select_i64_binop(Inst::Shr);
        assert_eq!(selected.insts[2], MachineInst::Shr { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_sar() {
        let (selected, x, y, r) = select_i64_binop(Inst::Sar);
        assert_eq!(selected.insts[2], MachineInst::Sar { dst: r, lhs: x, rhs: y });
    }

    fn select_f64_binop(inst_ctor: impl FnOnce(Value, Value) -> Inst) -> (SelectedFunction, Value, Value, Value) {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, inst_ctor(x, y), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
        let selected = select(&b.f);
        (selected, x, y, r)
    }

    /// Proves the SAME `Inst::Add` variant dispatches to FloatAdd (not
    /// IntAdd) for f64 operands -- the exact risk this task's dispatch
    /// exists to resolve correctly.
    #[test]
    fn select_lowers_float_add() {
        let (selected, x, y, r) = select_f64_binop(Inst::Add);
        assert_eq!(selected.insts[2], MachineInst::FloatAdd { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_float_sub() {
        let (selected, x, y, r) = select_f64_binop(Inst::Sub);
        assert_eq!(selected.insts[2], MachineInst::FloatSub { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_float_mul() {
        let (selected, x, y, r) = select_f64_binop(Inst::Mul);
        assert_eq!(selected.insts[2], MachineInst::FloatMul { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_float_div() {
        let (selected, x, y, r) = select_f64_binop(Inst::Div);
        assert_eq!(selected.insts[2], MachineInst::FloatDiv { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_int_neg() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Neg(x), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.insts[1], MachineInst::IntNeg { dst: r, src: x });
    }

    /// Proves Neg's OTHER branch: an f64 operand mints a synthetic mask
    /// temp and lowers to FloatNeg, not IntNeg -- the float counterpart to
    /// select_lowers_int_neg above, both exercising the same dispatching
    /// `Inst::Neg` arm.
    #[test]
    fn select_lowers_float_neg_via_a_synthetic_mask_temp() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, Inst::Neg(x), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        let mask_tmp = match &selected.insts[1] {
            MachineInst::LoadImmI64 { dst, imm } => {
                assert_eq!(*imm, i64::MIN);
                *dst
            }
            other => panic!("expected LoadImmI64 for the mask temp, got {:?}", other),
        };
        assert_eq!(
            selected.insts[2],
            MachineInst::FloatNeg { dst: r, src: x, mask_tmp }
        );
        assert_eq!(selected.synthetic_types.get(&mask_tmp), Some(&Ty::I64));
    }

    #[test]
    fn select_lowers_not() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::Not(x), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.insts[1], MachineInst::Not { dst: r, src: x });
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — every new test panics inside the `todo!("filled in by Tasks 2-6...")` arm.

- [ ] **Step 3: Extend `select_inst`'s match**

**Confirmed by reading the real source before writing this plan** (`crates/forge-ir/src/lower.rs:208-215`'s `lower_binary`): `Inst::Add`/`Sub`/`Mul`/`Div`/`Rem` are each a SINGLE shared `Inst` variant covering both `i64` and `f64` operands — `lower_binary` maps `BinaryOp::Add` to `Inst::Add(l, r)` unconditionally, with no type-based branching at the IR-construction level. The type is only recoverable from `Function.types`/`self.ty_of`. So these five arms (plus `Neg`, which is likewise shared between `i64` negation and `f64` negation) MUST dispatch on operand type here — there is no separate mechanism elsewhere distinguishing them. `And`/`Or`/`Xor`/`Not`/`Shl`/`Shr`/`Sar` do NOT need dispatch: per `ir.rs`'s own comment, they're shared between `i64` and `Bool` operands (a 1-bit boolean is representationally an i64's low bit), and both cases lower to the exact same `MachineInst` either way — there's no second, different lowering for `Bool` the way there is for `f64` arithmetic.

```rust
// crates/forge-x64/src/machine_inst.rs — inside select_inst's match, replace the `_ => todo!(...)` catch-all
// by inserting these arms ABOVE it (the catch-all stays, now covering only what Tasks 4-6 still need)

            Inst::Add(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Add never applies to Bool"),
            },
            Inst::Sub(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatSub { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntSub { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Sub never applies to Bool"),
            },
            Inst::Mul(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatMul { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntMul { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Mul never applies to Bool"),
            },
            Inst::Div(a, b) => match self.ty_of(*a) {
                Ty::F64 => self.insts.push(MachineInst::FloatDiv { dst, lhs: *a, rhs: *b }),
                Ty::I64 => self.insts.push(MachineInst::IntDiv { dst, lhs: *a, rhs: *b }),
                Ty::Bool => unreachable!("Div never applies to Bool"),
            },
            Inst::Rem(a, b) => {
                // Rem is I64-only (no FloatRem MachineInst exists -- forge's
                // language has no f64 remainder operator per the type
                // checker); asserted here rather than silently mis-lowering.
                debug_assert_eq!(self.ty_of(*a), Ty::I64, "Rem is I64-only");
                self.insts.push(MachineInst::IntRem { dst, lhs: *a, rhs: *b });
            }
            Inst::Neg(a) => match self.ty_of(*a) {
                Ty::F64 => {
                    let mask_tmp = self.fresh(Ty::I64);
                    self.insts.push(MachineInst::LoadImmI64 { dst: mask_tmp, imm: i64::MIN });
                    self.insts.push(MachineInst::FloatNeg { dst, src: *a, mask_tmp });
                }
                Ty::I64 => self.insts.push(MachineInst::IntNeg { dst, src: *a }),
                Ty::Bool => unreachable!("Neg never applies to Bool"),
            },
            Inst::And(a, b) => self.insts.push(MachineInst::And { dst, lhs: *a, rhs: *b }),
            Inst::Or(a, b) => self.insts.push(MachineInst::Or { dst, lhs: *a, rhs: *b }),
            Inst::Xor(a, b) => self.insts.push(MachineInst::Xor { dst, lhs: *a, rhs: *b }),
            Inst::Not(a) => self.insts.push(MachineInst::Not { dst, src: *a }),
            Inst::Shl(a, b) => self.insts.push(MachineInst::Shl { dst, lhs: *a, rhs: *b }),
            Inst::Shr(a, b) => self.insts.push(MachineInst::Shr { dst, lhs: *a, rhs: *b }),
            Inst::Sar(a, b) => self.insts.push(MachineInst::Sar { dst, lhs: *a, rhs: *b }),
```

This task's `Neg` arm already handles BOTH the `i64` and `f64` cases (including minting the mask temp for the float case, using the `fresh()` helper from Task 1) — Task 5 does NOT need to touch `Neg` again; it only adds `Abs` and `Fma`, which are new `Inst` variants this task doesn't cover.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: all 18 new tests pass (11 i64 binop/unop tests for `Add`/`Sub`/`Mul`/`Div`/`Rem`/`And`/`Or`/`Xor`/`Shl`/`Shr`/`Sar`, 4 float-dispatch tests for `Add`/`Sub`/`Mul`/`Div`, `select_lowers_int_neg` and `select_lowers_float_neg_via_a_synthetic_mask_temp` exercising both branches of the dispatching `Neg` arm, plus `select_lowers_not`), all 6 from Task 1 still pass (24 total in `machine_inst`'s test module).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): select() lowering for integer/float arithmetic (shared Inst variants), bitwise, and shift ops"
```

## Context for this task

`Add`/`Sub`/`Mul`/`Div`/`Rem`/`Neg` are the only operations in this whole plan needing `Ty`-based dispatch inside `select_inst` — every other `Inst` variant maps to exactly one `MachineInst` shape regardless of operand type (either because it's inherently type-specific already, like `Sqrt`, or because both possible operand types share the same lowering, like `And`/`Or`/`Xor`/`Not` for `i64`/`Bool`). This task resolves that dispatch for all six at once, so Task 3 (float-specific intrinsics) doesn't need to revisit `Add`/`Sub`/`Mul`/`Div` at all — they're already fully handled here for both types.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: Float arithmetic lowering

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block
// (select_f64_binop was already added by Task 2 — reuse it, don't redefine it)

    fn select_f64_unop(inst_ctor: impl FnOnce(Value) -> Inst) -> (SelectedFunction, Value, Value) {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, inst_ctor(x), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));
        let selected = select(&b.f);
        (selected, x, r)
    }

    #[test]
    fn select_lowers_float_min() {
        let (selected, x, y, r) = select_f64_binop(Inst::Min);
        assert_eq!(selected.insts[2], MachineInst::FloatMin { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_float_max() {
        let (selected, x, y, r) = select_f64_binop(Inst::Max);
        assert_eq!(selected.insts[2], MachineInst::FloatMax { dst: r, lhs: x, rhs: y });
    }

    #[test]
    fn select_lowers_sqrt() {
        let (selected, x, r) = select_f64_unop(Inst::Sqrt);
        assert_eq!(selected.insts[1], MachineInst::FloatSqrt { dst: r, src: x });
    }

    #[test]
    fn select_lowers_floor() {
        let (selected, x, r) = select_f64_unop(Inst::Floor);
        assert_eq!(
            selected.insts[1],
            MachineInst::FloatRound { dst: r, src: x, mode: crate::RoundMode::Floor }
        );
    }

    #[test]
    fn select_lowers_ceil() {
        let (selected, x, r) = select_f64_unop(Inst::Ceil);
        assert_eq!(
            selected.insts[1],
            MachineInst::FloatRound { dst: r, src: x, mode: crate::RoundMode::Ceil }
        );
    }

    #[test]
    fn select_lowers_round() {
        let (selected, x, r) = select_f64_unop(Inst::Round);
        assert_eq!(
            selected.insts[1],
            MachineInst::FloatRound { dst: r, src: x, mode: crate::RoundMode::Nearest }
        );
    }

    #[test]
    fn select_lowers_trunc() {
        let (selected, x, r) = select_f64_unop(Inst::Trunc);
        assert_eq!(
            selected.insts[1],
            MachineInst::FloatRound { dst: r, src: x, mode: crate::RoundMode::Truncate }
        );
    }
```

**IMPORTANT**: `crate::RoundMode` needs `PartialEq`/`Debug` to support `assert_eq!` here — this was already confirmed present during the design review (`RoundMode` derives `Clone, Copy, PartialEq, Eq, Debug` in `assembler.rs`), so no changes to `RoundMode` itself should be needed. If these tests fail to compile over this, that confirmation was wrong — investigate rather than assuming.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — every new test panics inside `todo!(...)`.

- [ ] **Step 3: Extend `select_inst`'s match**

```rust
// crates/forge-x64/src/machine_inst.rs — inside select_inst's match, add these arms

            Inst::Min(a, b) => self.insts.push(MachineInst::FloatMin { dst, lhs: *a, rhs: *b }),
            Inst::Max(a, b) => self.insts.push(MachineInst::FloatMax { dst, lhs: *a, rhs: *b }),
            Inst::Sqrt(a) => self.insts.push(MachineInst::FloatSqrt { dst, src: *a }),
            Inst::Floor(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Floor,
            }),
            Inst::Ceil(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Ceil,
            }),
            Inst::Round(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Nearest,
            }),
            Inst::Trunc(a) => self.insts.push(MachineInst::FloatRound {
                dst,
                src: *a,
                mode: crate::RoundMode::Truncate,
            }),
```

`Add`/`Sub`/`Mul`/`Div` are already fully handled by Task 2's dispatching arms (both the `i64` and `f64` branches) — this task does not touch them again. `Min`/`Max`/`Sqrt`/`Floor`/`Ceil`/`Round`/`Trunc` are unambiguous: per the language's type checker (`crates/forge-syntax/src/typeck.rs`, confirmed during design review), these intrinsics are `f64`-only, so no `Ty` dispatch is needed for them — each is its own dedicated `Inst` variant with only one possible operand type.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: all 7 new tests pass (`select_lowers_float_min`, `float_max`, `sqrt`, `floor`, `ceil`, `round`, `trunc`), all 24 from Tasks 1-2 still pass (31 total).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): select() lowering for float-only intrinsics (min/max/sqrt/rounding)"
```

## Context for this task

This task is narrower than originally scoped during design: Task 2 turned out to already need (and already implement) the full `i64`/`f64` dispatch for `Add`/`Sub`/`Mul`/`Div`/`Rem`/`Neg`, since those are shared `Inst` variants — confirmed by reading `crates/forge-ir/src/lower.rs` directly rather than assumed. This task only adds the operations that are genuinely NEW `Inst` variants with no integer counterpart at all.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: Comparisons and conversions

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    #[test]
    fn select_lowers_int_cmp() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(
            entry,
            Inst::Cmp { op: CmpOp::Lt, lhs: x, rhs: y },
            Ty::Bool,
            dummy_span(),
        );
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[2],
            MachineInst::IntCmp { op: CmpOp::Lt, dst: r, lhs: x, rhs: y }
        );
    }

    #[test]
    fn select_lowers_float_cmp() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(
            entry,
            Inst::Cmp { op: CmpOp::Lt, lhs: x, rhs: y },
            Ty::Bool,
            dummy_span(),
        );
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(
            selected.insts[2],
            MachineInst::FloatCmp { op: CmpOp::Lt, dst: r, lhs: x, rhs: y }
        );
    }

    #[test]
    fn select_lowers_i_to_f() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::I64 }, Ty::I64, dummy_span());
        let r = b.emit(entry, Inst::IToF(x), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.insts[1], MachineInst::IntToFloat { dst: r, src: x });
    }

    #[test]
    fn select_lowers_f_to_i() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, Inst::FToI(x), Ty::I64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        assert_eq!(selected.insts[1], MachineInst::FloatToInt { dst: r, src: x });
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -40`
Expected: FAIL — new tests panic inside `todo!(...)`.

- [ ] **Step 3: Extend `select_inst`'s match**

```rust
// crates/forge-x64/src/machine_inst.rs — inside select_inst's match, add these arms

            Inst::Cmp { op, lhs, rhs } => {
                let operand_ty = self.ty_of(*lhs);
                match operand_ty {
                    Ty::F64 => self.insts.push(MachineInst::FloatCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                    Ty::I64 | Ty::Bool => self.insts.push(MachineInst::IntCmp {
                        op: *op,
                        dst,
                        lhs: *lhs,
                        rhs: *rhs,
                    }),
                }
            }
            Inst::IToF(a) => self.insts.push(MachineInst::IntToFloat { dst, src: *a }),
            Inst::FToI(a) => self.insts.push(MachineInst::FloatToInt { dst, src: *a }),
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: all 4 new tests pass, all 31 from Tasks 1-3 still pass (35 total).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): select() lowering for comparisons and int/float conversions"
```

## Context for this task

`IntCmp`/`FloatCmp`'s dispatch reads the OPERAND's type (`self.ty_of(*lhs)`), not the destination's — `Cmp`'s destination is always `Ty::Bool` regardless of whether the comparison is between two `i64`s or two `f64`s, so dispatching on `dst`'s type would be a real bug (always selecting `IntCmp`, since `Bool` falls into the `I64 | Bool` arm). This mirrors 6e's `ucomisd_reg_reg` design point: float comparisons need the UNSIGNED `ConditionCode` variants (`Below`/`BelowOrEqual`/etc, not `Less`/`LessOrEqual`), a fact the eventual emission step (post-Phase-8) must handle when it sees `FloatCmp` vs `IntCmp` — not this task's concern, but worth knowing why the two are kept as separate `MachineInst` variants rather than one generic `Cmp` variant with an `is_float: bool` flag.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: `Abs`/`Neg` (float) and `Fma` decomposition

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    #[test]
    fn select_lowers_abs_via_a_synthetic_mask_temp() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, Inst::Abs(x), Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        // insts[0] = Param x, insts[1] = LoadImmI64 for the mask temp,
        // insts[2] = FloatAbs, insts[3] = Return.
        let mask_tmp = match &selected.insts[1] {
            MachineInst::LoadImmI64 { dst, imm } => {
                assert_eq!(*imm, 0x7FFF_FFFF_FFFF_FFFFi64);
                *dst
            }
            other => panic!("expected LoadImmI64 for the mask temp, got {:?}", other),
        };
        assert_eq!(
            selected.insts[2],
            MachineInst::FloatAbs { dst: r, src: x, mask_tmp }
        );
        assert_eq!(selected.synthetic_types.get(&mask_tmp), Some(&Ty::I64));
        // The mask temp's Value must not collide with any real IR value --
        // the highest real Value index is `r` (the Abs instruction itself).
        assert!(mask_tmp.0 > r.0);
    }

    #[test]
    fn select_lowers_fma_as_mul_then_add() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let y = b.emit(entry, Inst::Param { index: 1, ty: Ty::F64 }, Ty::F64, dummy_span());
        let z = b.emit(entry, Inst::Param { index: 2, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(entry, Inst::Fma { a: x, b: y, c: z }, Ty::F64, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        let selected = select(&b.f);

        // insts[0..3] = the three Params, insts[3] = FloatMul into a
        // synthetic temp, insts[4] = FloatAdd combining that temp with z.
        let mul_tmp = match &selected.insts[3] {
            MachineInst::FloatMul { dst, lhs, rhs } => {
                assert_eq!(*lhs, x);
                assert_eq!(*rhs, y);
                *dst
            }
            other => panic!("expected FloatMul, got {:?}", other),
        };
        assert_eq!(
            selected.insts[4],
            MachineInst::FloatAdd { dst: r, lhs: mul_tmp, rhs: z }
        );
        assert_eq!(selected.synthetic_types.get(&mul_tmp), Some(&Ty::F64));
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -40`
Expected: FAIL — new tests panic inside `todo!(...)`.

- [ ] **Step 3: Extend `select_inst`'s match**

```rust
// crates/forge-x64/src/machine_inst.rs — inside select_inst's match, add these arms

            Inst::Abs(a) => {
                let mask_tmp = self.fresh(Ty::I64);
                self.insts.push(MachineInst::LoadImmI64 {
                    dst: mask_tmp,
                    imm: 0x7FFF_FFFF_FFFF_FFFFi64,
                });
                self.insts.push(MachineInst::FloatAbs { dst, src: *a, mask_tmp });
            }
            Inst::Fma { a, b, c } => {
                let mul_tmp = self.fresh(Ty::F64);
                self.insts.push(MachineInst::FloatMul { dst: mul_tmp, lhs: *a, rhs: *b });
                self.insts.push(MachineInst::FloatAdd { dst, lhs: mul_tmp, rhs: *c });
            }
```

`Neg` (both `i64` and `f64` branches, including the float mask-temp sequence) is already fully implemented by Task 2 — this task doesn't touch it. Only `Abs` and `Fma` are new here.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: both new tests pass (`select_lowers_abs_via_a_synthetic_mask_temp`, `select_lowers_fma_as_mul_then_add`), all 35 from Tasks 1-4 still pass (37 total).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs
git commit -m "feat(forge-x64): select() lowering for Abs/Neg (float) via synthetic mask temps, Fma decomposition"
```

## Context for this task

This is the task the design doc calls out as needing the `fresh()` synthetic-value mechanism built in Task 1 — both `Abs` and `Fma` (and now `Neg` on floats) mint a temporary `Value` that has no entry in `func.types`/`func.spans`, recorded instead in `SelectedFunction::synthetic_types`. The two mask-temp tests assert `mask_tmp.0 > r.0` specifically to prove the synthetic value's index is genuinely beyond the real IR's `Value` range, not an accidental collision — this is the concrete, executable version of the design doc's "collision-free by construction" claim, so don't weaken or remove that assertion.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 6: `Phi` (no-op), `Call` (unimplemented), and final match exhaustiveness

**Files:**
- Modify: `crates/forge-x64/src/machine_inst.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/forge-x64/src/machine_inst.rs — add to the #[cfg(test)] mod tests block

    /// A diamond CFG (entry branches to then/else, both jump to merge,
    /// merge has a phi) -- confirms Phi produces NO MachineInst, per the
    /// design doc's explicit deferral of phi resolution to Phase 8.
    #[test]
    fn select_emits_nothing_for_phi() {
        let mut b = Builder::new();
        let entry = b.create_block();
        let then_b = b.create_block();
        let else_b = b.create_block();
        let merge = b.create_block();
        b.add_pred(then_b, entry);
        b.add_pred(else_b, entry);
        b.seal_block(entry);
        b.seal_block(then_b);
        b.seal_block(else_b);
        b.add_pred(merge, then_b);
        b.add_pred(merge, else_b);
        b.seal_block(merge);

        let cond = b.emit(entry, Inst::ConstBool(true), Ty::Bool, dummy_span());
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Branch {
            cond,
            then_: then_b,
            else_: else_b,
        });
        let t = b.emit(then_b, Inst::ConstI64(1), Ty::I64, dummy_span());
        b.f.blocks[then_b.0 as usize].term = Some(Terminator::Jump(merge));
        let e = b.emit(else_b, Inst::ConstI64(0), Ty::I64, dummy_span());
        b.f.blocks[else_b.0 as usize].term = Some(Terminator::Jump(merge));
        let phi = b.emit(
            merge,
            Inst::Phi { incoming: smallvec::smallvec![(then_b, t), (else_b, e)] },
            Ty::I64,
            dummy_span(),
        );
        b.f.blocks[merge.0 as usize].term = Some(Terminator::Return(phi));

        let selected = select(&b.f);

        // No MachineInst variant anywhere in the output should be a
        // stand-in "phi" op -- the only thing referencing `phi`'s Value at
        // all is the final Return.
        assert_eq!(selected.insts.last(), Some(&MachineInst::Return { value: phi }));
        let phi_producing_insts: Vec<_> = selected
            .insts
            .iter()
            .filter(|i| matches!(i, MachineInst::LoadImmI64 { dst, .. } if *dst == phi))
            .collect();
        assert!(
            phi_producing_insts.is_empty(),
            "Phi's destination Value must not be produced by any MachineInst in Phase 7a"
        );
    }

    #[test]
    #[should_panic(expected = "Phase 7e")]
    fn select_panics_on_call_with_a_clear_deferral_message() {
        let mut b = Builder::new();
        let entry = b.create_block();
        b.seal_block(entry);
        let x = b.emit(entry, Inst::Param { index: 0, ty: Ty::F64 }, Ty::F64, dummy_span());
        let r = b.emit(
            entry,
            Inst::Call { func: forge_ir::LibFunc::Sin, args: smallvec::smallvec![x] },
            Ty::F64,
            dummy_span(),
        );
        b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

        select(&b.f); // must panic
    }
```

**IMPORTANT**: this test file will need `smallvec` available — check whether `crates/forge-x64/Cargo.toml` needs a `[dev-dependencies]` entry for it (`smallvec.workspace = true`) or whether it's already reachable transitively through `forge-ir`'s public API (`SmallVec` appears in `Inst::Call`'s and `Inst::Phi`'s field types, so the type itself is reachable, but the `smallvec![...]` macro requires the crate to be a direct dependency to use unqualified — add it to `[dev-dependencies]` if `cargo build` complains about an unresolved `smallvec` macro/crate).

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -40`
Expected: FAIL — `select_emits_nothing_for_phi` panics inside `todo!(...)` rather than completing; `select_panics_on_call_with_a_clear_deferral_message` also panics inside `todo!(...)`, whose message doesn't contain "Phase 7e", so the `#[should_panic(expected = ...)]` assertion fails too (wrong panic message, not the right one).

- [ ] **Step 3: Replace the `_ => todo!(...)` catch-all with the final two arms**

```rust
// crates/forge-x64/src/machine_inst.rs — inside select_inst's match, replace the entire
// `_ => todo!("filled in by Tasks 2-6 of the Phase 7a plan"),` line with:

            Inst::Call { .. } => unimplemented!("libm call lowering ships in Phase 7e"),
            Inst::Phi { .. } => {
                // Deliberately emits nothing -- see the design doc's "φ
                // handling" section. This Inst's destination Value is
                // resolved entirely by Phase 8's SSA deconstruction.
            }
```

The `match` is now exhaustive with no wildcard arm — if a future `forge_ir::Inst` variant is added and this file isn't updated, `cargo build` fails here with a missing-match-arm error, not a silent gap. Confirm this compiles with no `_ =>` arm anywhere in `select_inst`.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -30`
Expected: both new tests pass, all 37 from Tasks 1-5 still pass (39 total).

- [ ] **Step 5: Run the FULL workspace test suite one more time**

Run: `cargo test --workspace 2>&1 | tail -60`
Expected: every test in every crate passes.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst.rs crates/forge-x64/Cargo.toml
git commit -m "feat(forge-x64): select() handling for Phi (no-op) and Call (deferred to 7e), exhaustive match"
```

## Context for this task

This is the last substantive task in Phase 7a — after this, `select_inst`'s match has a real arm (or an intentional `unimplemented!`) for every `forge_ir::Inst` variant, satisfying exit criterion #1 from the design doc. The exhaustiveness itself (no `_ =>` wildcard) is the actual safety property being tested here, not just the two new behaviors — if a reviewer or future task adds a wildcard arm "to make it compile faster," that defeats the whole point and should be flagged as a regression.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 7: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -60`
Expected: every test passes. Report the exact final count for `forge-x64` (should be 39 tests in `machine_inst`'s module, per this plan's running arithmetic — trust the actual `cargo test` output over this number if they diverge).

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Confirm `select_inst`'s match is genuinely exhaustive**

Run: `grep -n "_ =>" crates/forge-x64/src/machine_inst.rs` — expect NO matches inside `select_inst`'s body (a `_ =>` arm anywhere else, e.g. in `ty_of`'s `if`/`else`, is fine and expected; only `select_inst`'s own match must have zero wildcard arms).

- [ ] **Step 5: Report exit criteria status**

Confirm all 6 exit criteria from the design doc are met:
1. `MachineInst` enum exists in `crates/forge-x64/src/machine_inst.rs`, covering every `forge_ir::Inst` variant.
2. `select(&Function) -> SelectedFunction` exists, walks blocks in RPO, lowers every `Inst` variant except `Call` (clear panic message).
3. Synthetic `Value`s never collide with real IR `Value`s, recorded in `SelectedFunction::synthetic_types`.
4. Golden-sequence tests exist for every arithmetic/bitwise/shift/comparison/conversion family, `Abs`/`Neg`, `Fma`, CFG lowering, and `Phi`'s no-op behavior.
5. `cargo test --workspace` green, clippy/fmt clean.
6. No regressions in any Phase 6 `forge-x64` test or any other crate's tests.
