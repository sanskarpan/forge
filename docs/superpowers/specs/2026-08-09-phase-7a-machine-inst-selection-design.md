# Design: forge Phase 7a — `MachineInst` + Baseline Instruction Selection

**Status:** Approved for planning
**Scope:** The first sub-slice of CHECKLIST.md Phase 7 ("Instruction Selection & Prologue," 22 tasks) — the `MachineInst` enum and a baseline tree-tiling (one-IR-node-to-one-or-few-MachineInst) selector, covering every `forge_ir::Inst` variant except `Phi` (explicitly deferred to Phase 8, see below), `Call` (deferred to 7e — needs real ABI machinery), and `Fma` (lowered as `Mul` then `Add` until AVX/FMA3 lands, out of scope until Phase 10 per CHECKLIST's VEX/AVX section).
**Out of scope (deferred):** addressing-mode folding, `lea` synthesis, `Select`→`cmov`/blend diamond-pattern conversion (all 7b — genuine multi-node tree-tiling, not baseline 1:1 lowering), the constant pool and RIP-relative-loaded sign-mask constants (7c — 7a's `Abs`/`Neg` work correctly today by materializing the sign mask into a scratch GPR + `movq_gpr_to_xmm` inline, per 6e's `andpd_reg_reg`/`xorpd_reg_reg` doc comments; 7c is a later optimization, not a correctness gap), prologue/epilogue/ABI frame plumbing (7d), the libm call sequence (7e), actual register allocation (Phase 8), and the final MachineInst-to-bytes emission step that resolves two-address copies and calls `Assembler` methods (built once Phase 8 exists and real `PhysReg` assignments are known — see "Two-address fixup" below).

## Context: resolving two real ambiguities in CHECKLIST.md/SPEC.md

Two things needed resolving before this design was possible, both confirmed with the project owner:

1. **Phase 7 vs. Phase 8 ordering.** SPEC.md's pipeline diagram (§4) shows register allocation happening *before* instruction selection; CHECKLIST.md numbers the phases the other way, and its own bullet wording for each phase assumes the *other* phase's output already exists (Phase 7's prologue needs Phase 8's spill-slot count; Phase 8's coalescing needs Phase 7's two-address hints). Resolution: Phase 7 builds `MachineInst` entirely in terms of virtual registers (`forge_ir::Value`, reused directly — no new VReg type) and produces coalescing hints as *metadata*, not by inserting/eliding real copies. Phase 8 assigns real `PhysReg`s (and spill slots) to those virtual registers, honoring hints where possible. A final "emission" step — built at the end of Phase 8, since it's the first point real register assignments exist — walks `MachineInst` + the assignment and calls `Assembler` methods, deciding per-instruction whether a two-address copy is actually needed and emitting the real prologue/epilogue (7d's parameterized functions, now given real inputs).
2. **`forge_ir::Inst` has no `Load`/`Store`/`Select` variant**, despite CHECKLIST.md's Phase 7 bullets referencing "`Load{base, offset}` folds" and "`Select` → `cmov`." These describe things instruction selection *introduces itself*, not existing IR nodes: "addressing-mode folding" means fusing a multi-node arithmetic *tree* (e.g. `Add(Mul(b, ConstI64(k)), c)`) into a single `lea`'s effective-address computation; "Select" means recognizing a diamond CFG shape (`Branch` to two blocks that both jump to a common `Phi`-bearing merge block) and rewriting it as branchless `cmov`/blend code when profitable. Both are real, non-trivial pattern-matching work — correctly bucketed as 7b, not 7a.

**φ handling.** `forge_ir::Inst::Phi` is a real IR node, but CHECKLIST.md's Phase 7 task list never mentions lowering it, while Phase 8's "φ handling: an interval spans from the φ to all its incoming definitions" implies liveness analysis treats φ specially. Decision: Phase 7a's selector emits **no `MachineInst` for `Phi`** — a `Phi`'s destination `Value` and its incoming values are treated as the same virtual-register identity by every later stage until Phase 8, which resolves φs via classic SSA deconstruction (assign the same physical register/slot where possible; insert parallel-copy moves at predecessor block ends where coalescing isn't possible). This is standard SSA-based codegen practice and keeps Phase 7 from needing to understand register assignment at all. This strategy's safety depends on today's CFG having no critical edges — verified true for the current if/else-only lowering (`forge-ir/src/lower.rs`: every branch target has exactly one predecessor before merging) — but this is an *invariant of the current CFG shape*, not something enforced or checked anywhere yet. Phase 8's own design doc must either re-verify this invariant explicitly or add critical-edge splitting, especially once loops (`while`, currently a stretch goal per `lower.rs`) can introduce back-edges.

## Architecture

New file `crates/forge-x64/src/machine_inst.rs` (SPEC.md §17 explicitly places "instruction selection" inside `forge-x64`, alongside the encoder — no new crate). `assembler.rs` is purely about byte-level encoding and stays that way; `machine_inst.rs` is a new, separate responsibility (the file-per-responsibility split this project has followed since Phase 6's design docs).

`MachineInst` is a flat enum, one variant per real operation family — mirroring `forge_ir::Inst`'s own flat style (not an opcode-enum-plus-generic-shape like `AluOp`/`SseOp`, which exist specifically because the *encoder* needs to share literal byte-structure; `MachineInst` is a different abstraction layer with no such constraint). Every variant is in **3-address SSA form**: `dst` is always present and distinct from operand `Value`s, even for x86 ops that are destructive 2-address on real hardware (`Add`/`Sub`/etc.) — 7b attaches coalescing hints to these, but does not rewrite the form itself; only the final post-Phase-8 emission step decides whether a real copy is needed, based on whether the allocator actually placed `dst` and the relevant operand in the same physical location.

Where an `Inst` doesn't map to one real x86 operation (`Fma`, `Abs`, `Neg` for now), the selector mints **fresh synthetic `Value`s** for intermediate results, seeded from one past the IR's highest existing `Value` index (`Function.insts.len()` as a `u32`, since every `Value(i)` indexes into `insts`). This is the one new piece of state the selector owns: a `next_value: u32` counter, incremented each time a temporary is needed. These synthetic values never appear in the original `Function` and have no entry in `Function.types`/`Function.spans` — Phase 8 will need to track their `Ty` itself (the selector should return a `HashMap<Value, Ty>` or parallel structure for synthesized values, since the allocator needs to know GPR-vs-XMM class for every virtual register it assigns, and `PhysReg` currently has no class concept at all — a real gap this project's `reg.rs` doesn't fill yet, to be addressed in Phase 8, not here).

Selection walks blocks in **reverse postorder** (`forge_ir::dominance::reverse_postorder`, already implemented and exported — confirmed by reading `dominance.rs` directly, not assumed), and within each block, walks `BlockData.insts` in order, lowering each `Value`'s defining `Inst` to zero (`Phi`), one, or a short fixed sequence of `MachineInst`s.

## Components

### `MachineInst` enum (illustrative — full variant list finalized during planning)

```rust
use forge_ir::{Block, CmpOp, Value};

pub enum MachineInst {
    // Constants
    LoadImmI64 { dst: Value, imm: i64 },   // also used for Inst::ConstBool (0/1)
    LoadImmF64 { dst: Value, bits: u64 }, // via a synthetic GPR temp + movq_gpr_to_xmm

    // Integer arithmetic -- destructive (dst must end up == lhs's location)
    IntAdd { dst: Value, lhs: Value, rhs: Value },
    IntSub { dst: Value, lhs: Value, rhs: Value },
    IntMul { dst: Value, lhs: Value, rhs: Value },
    IntDiv { dst: Value, lhs: Value, rhs: Value }, // cqo + idiv; RAX/RDX-fixed, noted for Phase 8
    IntRem { dst: Value, lhs: Value, rhs: Value }, // same as IntDiv, takes RDX instead of RAX
    IntNeg { dst: Value, src: Value },
    And { dst: Value, lhs: Value, rhs: Value },
    Or { dst: Value, lhs: Value, rhs: Value },
    Xor { dst: Value, lhs: Value, rhs: Value },
    Not { dst: Value, src: Value },
    Shl { dst: Value, lhs: Value, rhs: Value },  // rhs is fixed to CL, noted for Phase 8
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
    FloatRound { dst: Value, src: Value, mode: forge_x64::RoundMode },
    // Abs/Neg: synthesize a mask temp (Value) + LoadImmI64 + a GPR->XMM move
    // + AndPd/XorPd -- see "Abs/Neg lowering" below for the exact sequence.
    FloatAbs { dst: Value, src: Value, mask_tmp: Value },
    FloatNeg { dst: Value, src: Value, mask_tmp: Value },

    // Comparisons -- resolved to a specific strategy at selection time
    IntCmp { op: CmpOp, dst: Value, lhs: Value, rhs: Value },   // cmp + setcc, signed codes
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
```

### `select(func: &Function) -> SelectedFunction`

```rust
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    /// forge_ir::Ty for every virtual register the selector introduced that
    /// ISN'T already in `func.types` (i.e. every synthetic temp) -- Phase 8
    /// needs this to know GPR-vs-XMM class for registers `func.types`
    /// doesn't cover. Real IR values look their Ty up in `func.types`
    /// directly; this map is ONLY for synthesized values.
    pub synthetic_types: std::collections::HashMap<Value, forge_ir::Ty>,
}

pub fn select(func: &Function) -> SelectedFunction { /* ... */ }
```

Walks `forge_ir::dominance::reverse_postorder(func)`, then each block's `insts` in order, matching on `func.insts[value.0 as usize]` and pushing the resulting `MachineInst`(s). Terminators (`func.blocks[block].term`) are lowered after the block's instructions.

### `Abs`/`Neg` lowering (the one genuinely multi-step baseline case)

```
FloatAbs { dst, src, mask_tmp }:
    LoadImmI64 { dst: mask_tmp, imm: 0x7FFF_FFFF_FFFF_FFFFi64 }  // clear sign bit
    // then, at emission time (post Phase 8): movq_gpr_to_xmm(mask_phys, mask_tmp_phys);
    // andpd_reg_reg(dst_phys, mask_phys) -- dst must already hold src's value
    // (a copy is inserted pre-andpd if coalescing didn't place src in dst's register)

FloatNeg { dst, src, mask_tmp }:
    LoadImmI64 { dst: mask_tmp, imm: i64::MIN }  // sign bit only, 0x8000...0000
    // then: movq_gpr_to_xmm + xorpd_reg_reg, same shape as FloatAbs
```

Both `mask_tmp`'s `LoadImmI64` is a normal `MachineInst` in the selected sequence (its `Value` is synthetic — added to `SelectedFunction::synthetic_types` as `Ty::I64`); the `movq_gpr_to_xmm`+`andpd`/`xorpd` pair is NOT modeled as separate `MachineInst`s here — `FloatAbs`/`FloatNeg` carry `mask_tmp` as a field precisely so the (post-Phase-8) emission step can synthesize that exact 2-instruction sequence once it knows `mask_tmp`'s and `dst`'s real registers. This keeps `MachineInst` a 1:1 semantic match with "compute abs/neg of this value" while still giving emission everything it needs.

### `Fma` lowering (decomposition, documented as temporary)

```
Fma { a, b, c } with dst == the Fma's own Value:
    FloatMul { dst: mul_tmp, lhs: a, rhs: b }   // mul_tmp: fresh synthetic Value, Ty::F64
    FloatAdd { dst: dst, lhs: mul_tmp, rhs: c }
```

Not bit-identical to a real hardware FMA (two roundings instead of one) — CHECKLIST.md's own FMA bullets (`vfmadd213sd`/`vfmadd231sd`, FMA3) live entirely under Phase 6's "VEX/AVX" subsection (🟡, deferred, no consumer built yet), so this decomposition is the correct interim behavior, not a shortcut being silently taken. Document this loudly in the code, not just this doc.

### `Call`/`Phi` — explicitly unimplemented in 7a

```rust
match inst {
    // ...
    Inst::Call { .. } => unimplemented!("libm call lowering ships in Phase 7e"),
    Inst::Phi { .. } => { /* no MachineInst emitted -- see "φ handling" above */ }
    // ...
}
```

An exhaustive `match` (not a wildcard `_ => ...`) is required so that adding a future `Inst` variant is a compile error here, not a silent gap.

## Testing

Golden-`MachineInst`-sequence tests (assert the exact `Vec<MachineInst>` `select()` produces for a small hand-built `Function`, using `forge_ir::builder`), covering: one test per arithmetic/bitwise/shift/comparison family (int and float), `IToF`/`FToI`, `Abs`/`Neg`'s multi-instruction shape, `Fma`'s decomposition, a straight-line 3-block CFG (`Jump`/`Branch`/`Return` lowering, block order matching RPO), and confirmation that `Phi` produces zero `MachineInst`s while its `Value` still appears validly as an operand elsewhere. No disassembly/byte-level testing at this stage — `MachineInst` is a pre-encoding representation; round-trip-via-`iced-x86` testing resumes once the post-Phase-8 emission step exists.

## Exit criteria

1. `MachineInst` enum exists in `crates/forge-x64/src/machine_inst.rs`, covering every `forge_ir::Inst` variant (exhaustive match, `Phi`/`Call` explicitly handled per the decisions above).
2. `select(&Function) -> SelectedFunction` exists, walks blocks in RPO, and correctly lowers every `Inst` variant except `Call` (which panics with a clear "ships in 7e" message) into `MachineInst`s using virtual registers.
3. Synthetic `Value`s (from `Fma`/`Abs`/`Neg` decomposition) never collide with real IR `Value`s and are recorded in `SelectedFunction::synthetic_types`.
4. Golden-sequence tests exist for every arithmetic/bitwise/shift/comparison/conversion family, `Abs`/`Neg`, `Fma`, and CFG lowering (`Jump`/`Branch`/`Return`), plus a test confirming `Phi` emits nothing.
5. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
6. No regressions in any of Phase 6's existing `forge-x64` tests or any other crate's tests.
