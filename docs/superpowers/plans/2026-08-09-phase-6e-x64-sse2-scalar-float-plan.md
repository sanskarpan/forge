# forge Phase 6e x86-64 SSE2 Scalar Float Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build all of CHECKLIST.md's "SSE2 scalar float" bullet-group in `forge-x64` — `movsd`/`movapd`/`movq`, the 7-operation `addsd`/`subsd`/`mulsd`/`divsd`/`sqrtsd`/`minsd`/`maxsd` family, `andpd`/`xorpd`, `ucomisd` (reusing 6c's existing `ConditionCode`/`setcc`/`jcc`/`cmovcc` machinery unmodified), `cvtsi2sd`/`cvttsd2si`, and `roundsd` — verified via the same golden-byte + `iced-x86` disassembler round-trip discipline established in Phases 6a-6d.

**Architecture:** All new methods live in `crates/forge-x64/src/assembler.rs`, reusing `rex()`/`modrm_reg()`/`modrm_mem()` exactly as before. The one genuinely new mechanism: a mandatory legacy prefix byte (`0x66` or `0xF2`) must be pushed *before* calling `rex()` — a real ordering rule nothing built in 6a-6d needed. `SseOp` (7 ops, shared `F2`-prefix structure) and `RoundMode` (4 modes) are small enums mirroring `AluOp`/`ShiftOp`'s justification; `movapd`/`movq`/`andpd`/`xorpd`/`ucomisd`/conversions stay standalone (each pair or singleton doesn't share enough structure to warrant an enum, matching 6d's `not_reg`/`neg_reg` precedent).

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only — already wired in Phase 6a, no Cargo.toml changes needed).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-6e-x64-sse2-scalar-float-design.md` — read this first.

**A note on running test counts:** every task below states an "Expected" pass count computed from the prior task's estimate, starting from the confirmed baseline of 14 lib + 72 integration tests at the end of Phase 6d. In every prior Phase 6 sub-slice, review rounds sometimes added extra tests beyond a task's original estimate, making later tasks' arithmetic stale. Treat every count in this plan as a best-effort estimate, not ground truth: always trust the actual output of `cargo test -p forge-x64`, and if a later task's baseline looks wrong, check `git log` for the actual test count in the prior task's final commit rather than assuming this plan's running arithmetic is right.

---

## Task 1: `movsd_reg_reg` / `movsd_reg_mem` / `movsd_mem_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn movsd_reg_reg_needs_no_rex_for_low_registers() {
    let mut a = Assembler::new();
    a.movsd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x10, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["movsd xmm0,xmm1"]);
}

#[test]
fn movsd_reg_reg_extended_register_sets_rex_r() {
    let mut a = Assembler::new();
    a.movsd_reg_reg(PhysReg::Xmm9, PhysReg::Xmm0);
    assert_eq!(a.code(), &[0xF2, 0x44, 0x0F, 0x10, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["movsd xmm9,xmm0"]);
}

#[test]
fn movsd_reg_mem_loads_from_memory() {
    let mut a = Assembler::new();
    a.movsd_reg_mem(PhysReg::Xmm0, PhysReg::Rcx, 8);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x10, 0x41, 0x08]);
    assert_eq!(disassemble(a.code()), vec!["movsd xmm0,[rcx+8]"]);
}

#[test]
fn movsd_mem_reg_stores_to_memory() {
    let mut a = Assembler::new();
    a.movsd_mem_reg(PhysReg::Rcx, 8, PhysReg::Xmm0);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x11, 0x41, 0x08]);
    // Confirms genuine STORE direction -- opcode 0x11, not 0x10.
    assert_eq!(disassemble(a.code()), vec!["movsd [rcx+8],xmm0"]);
}
```

**IMPORTANT — before trusting the disassembly strings above**: this is the FIRST time `iced-x86` disassembles an XMM-register instruction anywhere in this crate — its exact formatting for XMM operands and the `F2` prefix's mnemonic naming (`movsd`, not some alternate spelling) was not verified against a live compile when this plan was written. Verify all four empirically: run with the string assertions temporarily removed, confirm the golden bytes pass, then observe the actual `iced-x86` output and correct any string that doesn't match before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `movsd_reg_reg`/`movsd_reg_mem`/`movsd_mem_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add a new `impl Assembler` block at the end of the file, after the block containing `cqo`

impl Assembler {
    /// `movsd dst, src` -- F2 0F 10 /r, load direction (reg=dst, rm=src).
    /// REX.W is always false -- unused/undefined for this opcode.
    pub fn movsd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x10);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `movsd dst, [base + disp]` -- F2 0F 10 /r, load direction, reuses
    /// modrm_mem exactly like mov_reg_mem/lea_reg_mem do.
    pub fn movsd_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, base.encoding());
        self.code.push(0x0F);
        self.code.push(0x10);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }

    /// `movsd [base + disp], src` -- F2 0F 11 /r, store direction, the
    /// mirror image of movsd_reg_mem (0x11 not 0x10).
    pub fn movsd_mem_reg(&mut self, base: PhysReg, disp: i32, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, src.encoding(), 0, base.encoding());
        self.code.push(0x0F);
        self.code.push(0x11);
        self.modrm_mem(src.encoding(), base.encoding(), disp);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib tests unchanged + 76 integration tests: 72 existing + 4 new).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): movsd_reg_reg, movsd_reg_mem, movsd_mem_reg"
```

## Context for this task

This is the foundational task for the whole slice: the first XMM-register instruction, and the first instruction anywhere in this crate needing a mandatory legacy prefix byte before REX. `movsd_reg_mem`/`movsd_mem_reg` reuse `modrm_mem` exactly like `mov_reg_mem`/`mov_mem_reg` from 6a/6b did — no need to re-test `modrm_mem`'s rsp/rbp/r12/r13 special cases here, they're opcode-agnostic and already exhaustively proven. If `movsd_reg_reg_extended_register_sets_rex_r` fails, check the byte ORDER first: prefix (`0xF2`) must come before REX, which comes before the escape byte (`0x0F`) — a swapped order would produce a different-length or differently-structured (and likely invalid) instruction.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `movapd_reg_reg` / `movq_gpr_to_xmm` / `movq_xmm_to_gpr`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn movapd_reg_reg_uses_the_0x66_prefix() {
    let mut a = Assembler::new();
    a.movapd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x28, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["movapd xmm0,xmm1"]);
}

#[test]
fn movq_gpr_to_xmm_transfers_into_the_xmm_register() {
    let mut a = Assembler::new();
    a.movq_gpr_to_xmm(PhysReg::Xmm9, PhysReg::Rax);
    assert_eq!(a.code(), &[0x66, 0x4C, 0x0F, 0x6E, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["movq xmm9,rax"]);
}

/// Confirms genuine STORE direction (opcode 0x7E, not 0x6E) -- the
/// mirror image of movq_gpr_to_xmm, same pairing discipline as
/// mov_reg_mem/mov_mem_reg's load/store pair from 6a/6b.
#[test]
fn movq_xmm_to_gpr_transfers_into_the_gpr() {
    let mut a = Assembler::new();
    a.movq_xmm_to_gpr(PhysReg::Rax, PhysReg::Xmm9);
    assert_eq!(a.code(), &[0x66, 0x4C, 0x0F, 0x7E, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["movq rax,xmm9"]);
}
```

**IMPORTANT**: verify all three disassembly strings empirically before committing — don't guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `movapd_reg_reg`/`movq_gpr_to_xmm`/`movq_xmm_to_gpr` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `movsd_reg_reg`

impl Assembler {
    /// `movapd dst, src` -- 66 0F 28 /r. Register-register only -- its
    /// most common real use; a memory-operand form is a small future
    /// addition if ever needed, not built now.
    pub fn movapd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x28);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `movq dst(xmm), src(gpr)` -- 66 REX.W 0F 6E /r, load direction.
    /// REX.W matters here (and for movq_xmm_to_gpr/cvtsi2sd/cvttsd2si)
    /// since a real 64-bit GPR value is being moved -- unlike every
    /// other SSE2 method in this slice, where W is unused.
    pub fn movq_gpr_to_xmm(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x6E);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `movq dst(gpr), src(xmm)` -- 66 REX.W 0F 7E /r, store direction
    /// (rm=dst, reg=src) -- the mirror image of movq_gpr_to_xmm.
    pub fn movq_xmm_to_gpr(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(0x0F);
        self.code.push(0x7E);
        self.modrm_reg(src.encoding(), dst.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 76 existing integration + 3 new = 79 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): movapd_reg_reg, movq_gpr_to_xmm, movq_xmm_to_gpr"
```

## Context for this task

`movq_gpr_to_xmm`/`movq_xmm_to_gpr` aren't a literal "swap the same two arguments" pair the way `imul_reg_reg`'s two direction-check tests are (their argument roles are asymmetric: one takes `(xmm, gpr)`, the other `(gpr, xmm)`) — instead, both tests use `Xmm9`/`Rax` and confirm the opcode byte (`0x6E` vs `0x7E`) and resulting mnemonic/operand order are each correct for their own direction. If either fails, check that the right operand (`dst` or `src`, per that specific method's role) landed in ModRM.reg vs ModRM.rm — a swap here would still produce a plausible-looking 5-byte instruction, just with the wrong mnemonic or operand order.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `SseOp` + `sse_reg_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

use forge_x64::SseOp;

#[test]
fn sse_reg_reg_add() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Add, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x58, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["addsd xmm0,xmm1"]);
}

#[test]
fn sse_reg_reg_sub() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Sub, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x5C, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["subsd xmm0,xmm1"]);
}

#[test]
fn sse_reg_reg_mul() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Mul, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x59, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["mulsd xmm0,xmm1"]);
}

#[test]
fn sse_reg_reg_div() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Div, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x5E, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["divsd xmm0,xmm1"]);
}

#[test]
fn sse_reg_reg_sqrt() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Sqrt, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x51, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["sqrtsd xmm0,xmm1"]);
}

/// minsd/maxsd are NOT commutative w.r.t. NaN -- this test only exists
/// to prove the encoding (opcode 0x5D) is correct, not to demonstrate
/// that semantic fact, which belongs to instruction-selection/the
/// interpreter, not the encoder.
#[test]
fn sse_reg_reg_min() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Min, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x5D, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["minsd xmm0,xmm1"]);
}

#[test]
fn sse_reg_reg_max() {
    let mut a = Assembler::new();
    a.sse_reg_reg(SseOp::Max, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0xF2, 0x0F, 0x5F, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["maxsd xmm0,xmm1"]);
}
```

**IMPORTANT**: verify all 7 disassembly strings empirically before committing — don't guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `SseOp`/`sse_reg_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add near AluOp/ConditionCode/ShiftOp, above the `#[cfg(test)]` module

/// A scalar-double arithmetic operation sharing identical F2-prefix,
/// 0F-escape structure, differing only by the final opcode byte -- the
/// same justification AluOp/ShiftOp have for existing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SseOp {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    Min,
    Max,
}

impl SseOp {
    fn opcode(self) -> u8 {
        match self {
            SseOp::Add => 0x58,
            SseOp::Sub => 0x5C,
            SseOp::Mul => 0x59,
            SseOp::Div => 0x5E,
            SseOp::Sqrt => 0x51,
            SseOp::Min => 0x5D,
            SseOp::Max => 0x5F,
        }
    }
}

impl Assembler {
    /// `op dst, src` -- F2 0F <op.opcode()> /r, load direction.
    /// minsd/maxsd are NOT commutative with respect to NaN (matching
    /// CHECKLIST's explicit warning and this project's interpreter's
    /// existing semantics) -- the encoder doesn't need to do anything
    /// special about this, but it's a real correctness fact worth
    /// documenting for whoever calls this with Min/Max.
    pub fn sse_reg_reg(&mut self, op: SseOp, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(op.opcode());
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

- [ ] **Step 4: Export `SseOp` from `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, ShiftOp, SseOp};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 79 existing integration + 7 new = 86 integration).

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): SseOp + sse_reg_reg for addsd/subsd/mulsd/divsd/sqrtsd/minsd/maxsd"
```

## Context for this task

All 7 tests use the same `Xmm0`/`Xmm1` pair deliberately — Task 1 already proved REX/extended-register handling works correctly for this exact prefix+escape+modrm shape, so re-testing that per operation here would be redundant. Each test's only real job is confirming its specific opcode byte is correct; if one fails, the bug is almost certainly a transposed digit in `SseOp::opcode()`'s match arms, not in `sse_reg_reg` itself (which is identical for every operation).

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: `andpd_reg_reg` / `xorpd_reg_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn andpd_reg_reg_encodes_correctly() {
    let mut a = Assembler::new();
    a.andpd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x54, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["andpd xmm0,xmm1"]);
}

#[test]
fn xorpd_reg_reg_encodes_correctly() {
    let mut a = Assembler::new();
    a.xorpd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x57, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["xorpd xmm0,xmm1"]);
}
```

**IMPORTANT**: verify both disassembly strings empirically before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `andpd_reg_reg`/`xorpd_reg_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `movsd_reg_reg`

impl Assembler {
    /// `andpd dst, src` -- 66 0F 54 /r. Raw bitwise-AND primitive, used
    /// (by a caller, not this method) to implement float `abs` by
    /// clearing the sign bit against a materialized sign-mask constant.
    /// This method does NOT materialize any mask itself -- that's
    /// instruction-selection's job (mov_reg_imm + movq_gpr_to_xmm),
    /// matching this crate's established "thin composable primitives"
    /// philosophy (see idiv_reg's cqo precondition, setcc's undone
    /// zero-extension).
    pub fn andpd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x54);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `xorpd dst, src` -- 66 0F 57 /r. Same raw-primitive philosophy as
    /// andpd_reg_reg, used to implement float `neg` by flipping the sign
    /// bit against a materialized mask.
    pub fn xorpd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x57);
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 86 existing integration + 2 new = 88 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): andpd_reg_reg, xorpd_reg_reg"
```

## Context for this task

These are deliberately raw primitives, not composed `abs_reg`/`neg_reg` helpers (per the design doc's explicit scope decision) — a future instruction-selection layer materializes the sign-mask constant via `mov_reg_imm` + `movq_gpr_to_xmm` and then calls `andpd_reg_reg`/`xorpd_reg_reg` against it. Neither method needs to know anything about mask values; they're generic 2-XMM-operand bitwise ops.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: `ucomisd_reg_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn ucomisd_reg_reg_encodes_correctly() {
    let mut a = Assembler::new();
    a.ucomisd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x2E, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["ucomisd xmm0,xmm1"]);
}

/// Demonstrates the unsigned-condition-code usage the doc comment
/// requires: ucomisd sets flags the same way an UNSIGNED integer cmp
/// does, so `setcc` after it must use an unsigned ConditionCode
/// (Below/BelowOrEqual/Above/AboveOrEqual/Equal/NotEqual), never a
/// signed one (Less/Greater/etc). This is as close as a disassembly-only
/// test suite can get to proving the semantic claim -- it can't verify
/// the actual runtime flag behavior, only that the combination encodes
/// as intended.
#[test]
fn ucomisd_followed_by_setcc_below_encodes_the_less_than_comparison() {
    let mut a = Assembler::new();
    a.ucomisd_reg_reg(PhysReg::Xmm0, PhysReg::Xmm1);
    a.setcc(ConditionCode::Below, PhysReg::Rax);
    assert_eq!(
        a.code(),
        &[0x66, 0x0F, 0x2E, 0xC1, 0x0F, 0x92, 0xC0]
    );
    assert_eq!(
        disassemble(a.code()),
        vec!["ucomisd xmm0,xmm1", "setb al"]
    );
}
```

**IMPORTANT**: verify both disassembly strings empirically before committing — the `setb` mnemonic naming for `ConditionCode::Below` in particular was not checked against a live compile when this plan was written.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `ucomisd_reg_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `movsd_reg_reg`

impl Assembler {
    /// `ucomisd a, b` -- 66 0F 2E /r. Compares `a` and `b`, sets EFLAGS.
    ///
    /// IMPORTANT: ucomisd sets ZF/PF/CF the same way an UNSIGNED integer
    /// `cmp` does, not the SF/OF-based signed comparison flags. Use the
    /// unsigned ConditionCode variants with setcc/jcc/cmovcc after this
    /// (Below/BelowOrEqual/Above/AboveOrEqual/Equal/NotEqual), NOT the
    /// signed ones (Less/LessOrEqual/Greater/GreaterOrEqual) -- using the
    /// signed codes after a float comparison produces a plausible-looking
    /// but wrong result. No changes are needed in setcc/jcc/cmovcc
    /// themselves; this is purely a caller-facing usage note.
    pub fn ucomisd_reg_reg(&mut self, a: PhysReg, b: PhysReg) {
        self.code.push(0x66);
        self.rex(false, a.encoding(), 0, b.encoding());
        self.code.push(0x0F);
        self.code.push(0x2E);
        self.modrm_reg(a.encoding(), b.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 88 existing integration + 2 new = 90 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): ucomisd_reg_reg, reusing setcc/jcc/cmovcc's ConditionCode unmodified"
```

## Context for this task

No changes to `ConditionCode`, `setcc`, `jcc`, or `cmovcc` are needed or expected in this task — the whole point is that 6c's existing machinery already works for float comparisons, given the right (unsigned) condition codes. If you find yourself wanting to add a new enum or method here, stop and re-read the design doc's Architecture section — that would mean something was misunderstood.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 6: `cvtsi2sd` / `cvttsd2si`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

/// Uses R9 as the GPR operand so REX.B correctly threading through as
/// the `rm` field is genuinely exercised, not just assumed.
#[test]
fn cvtsi2sd_converts_gpr_to_xmm() {
    let mut a = Assembler::new();
    a.cvtsi2sd(PhysReg::Xmm0, PhysReg::R9);
    assert_eq!(a.code(), &[0xF2, 0x49, 0x0F, 0x2A, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["cvtsi2sd xmm0,r9"]);
}

/// Uses R9 as the GPR operand so REX.R correctly threading through as
/// the `reg` field is genuinely exercised (opposite REX bit from
/// cvtsi2sd's test, since the GPR is the destination here, not the
/// source -- a real place to get the direction backward).
#[test]
fn cvttsd2si_converts_xmm_to_gpr_truncating() {
    let mut a = Assembler::new();
    a.cvttsd2si(PhysReg::R9, PhysReg::Xmm0);
    assert_eq!(a.code(), &[0xF2, 0x4C, 0x0F, 0x2C, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["cvttsd2si r9,xmm0"]);
}
```

**IMPORTANT**: verify both disassembly strings empirically before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `cvtsi2sd`/`cvttsd2si` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `movsd_reg_reg`

impl Assembler {
    /// `cvtsi2sd dst(xmm), src(gpr)` -- F2 REX.W 0F 2A /r, load direction
    /// (reg=dst xmm, rm=src gpr). REX.W selects the 64-bit GPR source
    /// form (forge's i64), matching the AAPCS64/SysV convention this
    /// project always widens to.
    pub fn cvtsi2sd(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x2A);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `cvttsd2si dst(gpr), src(xmm)` -- F2 REX.W 0F 2C /r, load
    /// direction with the GPR as ModRM.reg (the destination) this time --
    /// direction is opposite to cvtsi2sd's, a real place to get backward.
    /// Truncating (toward zero), NOT rounding -- cvtsd2si (a different
    /// opcode, 0x2D) is the rounding variant and isn't built here.
    pub fn cvttsd2si(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x2C);
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 90 existing integration + 2 new = 92 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): cvtsi2sd, cvttsd2si"
```

## Context for this task

Both methods have the identical code shape (`rex(true, dst.encoding(), 0, src.encoding())` then `modrm_reg(dst.encoding(), src.encoding())`) — the only difference is the opcode byte (`0x2A` vs `0x2C`) and which conceptual register (GPR or XMM) plays the "dst"/"reg" role for that specific instruction. If either test fails, the bug is most likely a copy-paste that didn't adjust which physical register lands in `dst`'s slot for that instruction's actual defined semantics — re-read the doc comments' "reg=X" notes carefully.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 7: `RoundMode` + `roundsd`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

use forge_x64::RoundMode;

#[test]
fn roundsd_nearest() {
    let mut a = Assembler::new();
    a.roundsd(RoundMode::Nearest, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x08]);
    // NOTE: verify this string empirically -- this crate's established
    // pattern (6b/6c) is that iced-x86 sometimes renders small
    // immediates in hex, sometimes decimal, and the exact threshold
    // isn't known; this was not checked against a live compile when
    // this plan was written.
    assert_eq!(disassemble(a.code()), vec!["roundsd xmm0,xmm1,8"]);
}

#[test]
fn roundsd_floor() {
    let mut a = Assembler::new();
    a.roundsd(RoundMode::Floor, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x09]);
    // NOTE: verify this string empirically, same caveat as above.
    assert_eq!(disassemble(a.code()), vec!["roundsd xmm0,xmm1,9"]);
}

#[test]
fn roundsd_ceil() {
    let mut a = Assembler::new();
    a.roundsd(RoundMode::Ceil, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x0A]);
    // NOTE: verify this string empirically -- 0x0A (10 decimal) is
    // exactly the kind of value that has flipped between hex and
    // decimal rendering in past findings; treat this as fully unverified.
    assert_eq!(disassemble(a.code()), vec!["roundsd xmm0,xmm1,0Ah"]);
}

#[test]
fn roundsd_truncate() {
    let mut a = Assembler::new();
    a.roundsd(RoundMode::Truncate, PhysReg::Xmm0, PhysReg::Xmm1);
    assert_eq!(a.code(), &[0x66, 0x0F, 0x3A, 0x0B, 0xC1, 0x0B]);
    // NOTE: verify this string empirically, same caveat as roundsd_ceil.
    assert_eq!(disassemble(a.code()), vec!["roundsd xmm0,xmm1,0Bh"]);
}
```

**CRITICAL**: all four disassembly strings are genuinely unverified guesses with real, flagged uncertainty about hex-vs-decimal rendering for these specific small immediate values. Verify all four empirically before committing — do not trust any of them as written. This is the most encoding-novel task in the slice (the only 3-byte opcode plus immediate in this whole plan) — if the golden bytes themselves are wrong, check the control-byte math first (`mode | 0x08`) before suspecting the opcode/ModRM structure, which is otherwise identical to every other `sse_reg_reg`-shaped method.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `RoundMode`/`roundsd` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add near SseOp, above the `#[cfg(test)]` module

/// The four rounding modes CHECKLIST asks for (floor/ceil/round/trunc).
/// `roundsd`'s control byte also always sets bit 3 (0x08, "suppress
/// precision exception") -- the standard convention every mainstream
/// compiler uses, since without it a rounding operation that loses
/// precision raises a floating-point exception most code doesn't want.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundMode {
    Nearest,
    Floor,
    Ceil,
    Truncate,
}

impl RoundMode {
    fn control_byte(self) -> u8 {
        let mode = match self {
            RoundMode::Nearest => 0x00,
            RoundMode::Floor => 0x01,
            RoundMode::Ceil => 0x02,
            RoundMode::Truncate => 0x03,
        };
        mode | 0x08
    }
}

impl Assembler {
    /// `roundsd dst, src, mode` -- 66 0F 3A 0B /r ib. SSE4.1, not pure
    /// SSE2 (CHECKLIST's own bullet notes this) -- a 3-byte opcode
    /// (0F 3A escape + 0B) plus an immediate control byte, the most
    /// novel encoding shape in this slice. Runtime CPUID feature
    /// detection for SSE4.1 availability is a separate, later concern
    /// (this task only builds the encoder).
    pub fn roundsd(&mut self, mode: RoundMode, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x3A);
        self.code.push(0x0B);
        self.modrm_reg(dst.encoding(), src.encoding());
        self.code.push(mode.control_byte());
    }
}
```

- [ ] **Step 4: Export `RoundMode` from `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, RoundMode, ShiftOp, SseOp};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 92 existing integration + 4 new = 96 integration).

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): RoundMode + roundsd"
```

## Context for this task

This is the last substantive task in the slice, and the riskiest for a different reason than `lea_reg_scaled` was in 6d (that was risky because of new bit-level machinery; this is risky because the immediate operand's exact disassembly format is genuinely unpredictable from this plan's author's knowledge alone). Do not skip the empirical verification step here even if the golden bytes pass on the first try — the string assertions are a completely separate risk from the byte assertions, and this task's whole point is nailing down real, tested behavior rather than a plausible guess.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 8: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including all of `forge-x64`'s new tests. Report the exact final counts — per this plan's header note, trust the actual run over the plan's per-task arithmetic.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Confirm no regressions in 6a's/6b's/6c's/6d's work**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | tail -10` (confirm every named test family from 6a through 6d is still present and passing alongside 6e's new tests — `mov_*`/`jmp`/label-fixup from 6a, `alu_*`/`imul_*`/`mov_reg_imm`/`mov_mem_reg` from 6b, `test_reg_*`/`setcc_*`/`cmovcc_*`/`jcc_*` from 6c, `not_reg`/`neg_reg`/`inc_reg`/`dec_reg`/`shift_*`/`lea_*`/`imul128_reg`/`idiv_reg`/`cqo` from 6d) and `make spike` (confirm the Phase 0 day-one spike still works).

- [ ] **Step 5: Report exit criteria status**

Confirm all 9 exit criteria from the design doc are met:
1. `movsd_reg_reg`/`movsd_reg_mem`/`movsd_mem_reg` exist and pass tests, including a memory-operand case. ✅ (Task 1)
2. `movapd_reg_reg` and `movq_gpr_to_xmm`/`movq_xmm_to_gpr` exist; the movq pair's direction is tested. ✅ (Task 2)
3. `SseOp` and `sse_reg_reg` exist and pass tests for all 7 operations. ✅ (Task 3)
4. `andpd_reg_reg`/`xorpd_reg_reg` exist and pass tests. ✅ (Task 4)
5. `ucomisd_reg_reg` exists, is tested, and its unsigned-condition-code usage is demonstrated in a test combining it with `setcc`. ✅ (Task 5)
6. `cvtsi2sd`/`cvttsd2si` exist; both direction and REX.W are tested. ✅ (Task 6)
7. `RoundMode` and `roundsd` exist and pass tests for all 4 modes. ✅ (Task 7)
8. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 1-3)
9. No regressions in 6a's/6b's/6c's/6d's existing tests or any other crate's tests. ✅ (Step 4)
