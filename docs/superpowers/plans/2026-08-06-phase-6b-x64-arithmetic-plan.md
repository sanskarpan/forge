# forge Phase 6b x86-64 Arithmetic/Logic Instructions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the arithmetic/logic instruction family in `forge-x64` — group-1 ops (`add`/`or`/`and`/`sub`/`xor`, r/r and r/imm), `imul` (two- and three-operand forms), and `mov`'s remaining forms (register-immediate load, memory store) — verified via the same golden-byte + `iced-x86` disassembler round-trip discipline established in Phase 6a.

**Architecture:** All new methods live in `crates/forge-x64/src/assembler.rs`, built entirely on 6a's existing `rex()`/`modrm_reg()`/`modrm_mem()`/`DispMode` machinery. A new `AluOp` enum (`Add`/`Or`/`And`/`Sub`/`Xor`) carries each operation's opcode-extension digit and r/r opcode, consumed by two generic methods (`alu_reg_reg`, `alu_reg_imm`) instead of ten near-duplicate ones. `imul` and `mov`'s new forms get their own methods since they don't share group-1's encoding shape.

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only — already wired in Phase 6a, no Cargo.toml changes needed).

**Design doc:** `docs/superpowers/specs/2026-08-05-phase-6b-x64-arithmetic-design.md` — read this first.

---

## Task 1: `AluOp` + `alu_reg_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

use forge_x64::AluOp;

#[test]
fn alu_reg_reg_add() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::Add, PhysReg::Rax, PhysReg::Rbx);
    assert_eq!(a.code(), &[0x48, 0x01, 0xD8]);
    assert_eq!(disassemble(a.code()), vec!["add rax,rbx"]);
}

#[test]
fn alu_reg_reg_or_needs_rex_b_for_extended_destination() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::Or, PhysReg::R12, PhysReg::Rax);
    assert_eq!(a.code(), &[0x49, 0x09, 0xC4]);
    assert_eq!(disassemble(a.code()), vec!["or r12,rax"]);
}

#[test]
fn alu_reg_reg_and_needs_rex_r_for_extended_source() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::And, PhysReg::Rax, PhysReg::R9);
    assert_eq!(a.code(), &[0x4C, 0x21, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["and rax,r9"]);
}

#[test]
fn alu_reg_reg_sub_still_emits_rex_w_when_no_other_rex_bit_is_needed() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::Sub, PhysReg::Rbx, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0x29, 0xC3]);
    assert_eq!(disassemble(a.code()), vec!["sub rbx,rax"]);
}

#[test]
fn alu_reg_reg_xor_same_register_is_the_zero_idiom() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::Xor, PhysReg::Rax, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0x31, 0xC0]);
    assert_eq!(disassemble(a.code()), vec!["xor rax,rax"]);
}
```

**IMPORTANT — before trusting the disassembly strings above**: they were hand-derived by analogy with 6a's `mov_reg_reg` tests (same REX/ModRM shape, different opcode byte) and cross-checked against standard x86-64 encoding references, but — per this project's established discipline — you must still empirically verify them: run with the string assertions temporarily removed, confirm the golden bytes pass, then observe the actual `iced-x86` output (`cargo test -p forge-x64 --test round_trip -- --nocapture` with a temporary `println!`) and correct any string that doesn't match before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `AluOp`/`alu_reg_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add near the top of the file, alongside other public types

/// A group-1 arithmetic/logic operation: `add`/`or`/`and`/`sub`/`xor` share
/// real x86-64 encoding structure (the same "ModRM.reg as opcode
/// extension" trick for immediate forms, r/r opcodes offset by a fixed
/// stride) -- this enum carries each operation's two opcode facts instead
/// of duplicating the encoding logic five times. `adc`(/2) and `sbb`(/3)
/// exist in the same family but aren't part of this crate's instruction
/// set yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AluOp {
    Add,
    Or,
    And,
    Sub,
    Xor,
}

impl AluOp {
    /// The ModRM.reg "opcode extension" digit used by the immediate forms
    /// (0x81/0x83 /n).
    fn extension(self) -> u8 {
        match self {
            AluOp::Add => 0,
            AluOp::Or => 1,
            AluOp::And => 4,
            AluOp::Sub => 5,
            AluOp::Xor => 6,
        }
    }

    /// The direct r/r opcode -- store-direction, same convention as
    /// `mov_reg_reg`'s 0x89 (ModRM.rm is the destination, ModRM.reg is
    /// the source).
    fn rr_opcode(self) -> u8 {
        match self {
            AluOp::Add => 0x01,
            AluOp::Or => 0x09,
            AluOp::And => 0x21,
            AluOp::Sub => 0x29,
            AluOp::Xor => 0x31,
        }
    }
}

impl Assembler {
    /// `op dst, src` -- e.g. `add rax, rbx`. Same shape as `mov_reg_reg`:
    /// ModRM.rm is the destination, ModRM.reg is the source.
    pub fn alu_reg_reg(&mut self, op: AluOp, dst: PhysReg, src: PhysReg) {
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(op.rr_opcode());
        self.modrm_reg(src.encoding(), dst.encoding());
    }
}
```

- [ ] **Step 4: Export `AluOp` from `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{AluOp, Assembler, Label};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib tests, unchanged + 17 existing integration tests + 5 new = 22 integration tests). Remember Step 1's instruction: verify the `disassemble(...)` string literals empirically before trusting this "pass."

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): AluOp + alu_reg_reg for add/or/and/sub/xor"
```

## Context for this task

The 5 test cases deliberately mirror 6a's `mov_reg_reg` test coverage pattern (REX.B via an extended destination, REX.R via an extended source, REX.W-only with no other extension needed) plus one operation-specific case (`xor rax,rax`, the classic "zero a register" idiom, which is also a `reg==rm` degenerate-case check). All golden bytes were hand-derived from the standard x86-64 group-1 opcode table (ADD=0x00-03, OR=0x08-0B, AND=0x20-23, SUB=0x28-2B, XOR=0x30-33) and cross-checked by analogy with 6a's already-verified `mov_reg_reg` bytes. If your empirical disassembly disagrees with the golden BYTES (not just the guessed strings), that's a real bug in `AluOp`'s opcode tables or `alu_reg_reg` to investigate, not something to paper over.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `alu_reg_imm`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

**Note (found during Task 1):** `AluOp::extension()` currently carries a temporary `#[allow(dead_code)]` — Task 1's `alu_reg_reg` only calls `AluOp::rr_opcode()`, so `extension()` has no caller yet, which trips `cargo clippy --workspace -- -D warnings`'s dead-code lint without the allow. `alu_reg_imm()` below is `extension()`'s first real call site — remove the `#[allow(dead_code)]` from `AluOp::extension()` as part of this task, once `alu_reg_imm()` calls it for real.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn alu_reg_imm_add_uses_the_compact_imm8_form_when_it_fits() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::Add, PhysReg::Rax, 5);
    assert_eq!(a.code(), &[0x48, 0x83, 0xC0, 0x05]);
    assert_eq!(disassemble(a.code()), vec!["add rax,5"]);
}

#[test]
fn alu_reg_imm_sub_imm8_handles_a_negative_value() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::Sub, PhysReg::Rbx, -1);
    assert_eq!(a.code(), &[0x48, 0x83, 0xEB, 0xFF]);
    // Confirmed empirically: iced-x86 renders a sign-extended negative
    // immediate as its 64-bit hex pattern, not decimal.
    assert_eq!(disassemble(a.code()), vec!["sub rbx,0FFFFFFFFFFFFFFFFh"]);
}

#[test]
fn alu_reg_imm_and_falls_back_to_imm32_when_it_does_not_fit_in_i8() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::And, PhysReg::Rax, 1000);
    assert_eq!(a.code(), &[0x48, 0x81, 0xE0, 0xE8, 0x03, 0x00, 0x00]);
    // Confirmed empirically: iced-x86 renders it in hex ("3E8h"), not
    // decimal ("1000"), consistent with 6a's earlier finding.
    assert_eq!(disassemble(a.code()), vec!["and rax,3E8h"]);
}
```

**IMPORTANT**: both `NOTE` comments above flag real, unverified guesses — per Step 1's usual instruction (temporarily remove the string assertions, confirm golden bytes pass, observe the real `iced-x86` output, correct the strings), resolve both before committing. Do not guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `alu_reg_imm` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `alu_reg_reg`

impl Assembler {
    /// `op dst, imm` -- e.g. `add rax, 5`. ModRM.reg holds the opcode
    /// extension digit (not a register), ModRM.rm holds dst. Auto-selects
    /// the compact `83 /n ib` (imm8) form when `imm` fits in i8, else the
    /// general `81 /n id` (imm32) form -- both sign-extend to 64 bits.
    pub fn alu_reg_imm(&mut self, op: AluOp, dst: PhysReg, imm: i32) {
        self.rex(true, 0, 0, dst.encoding());
        if let Ok(imm8) = i8::try_from(imm) {
            self.code.push(0x83);
            self.modrm_reg(op.extension(), dst.encoding());
            self.code.push(imm8 as u8);
        } else {
            self.code.push(0x81);
            self.modrm_reg(op.extension(), dst.encoding());
            self.code.extend_from_slice(&imm.to_le_bytes());
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 22 existing integration + 3 new = 25 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): alu_reg_imm with imm8/imm32 auto-selection"
```

## Context for this task

The three test cases exercise both encoding paths: `alu_reg_imm_add_uses_the_compact_imm8_form_when_it_fits` and `alu_reg_imm_sub_imm8_handles_a_negative_value` both hit the imm8 path (positive and negative), `alu_reg_imm_and_falls_back_to_imm32_when_it_does_not_fit_in_i8` hits the imm32 fallback. All three golden byte sequences were hand-derived from the group-1 immediate opcode table and cross-checked against 6a's already-verified `disp_mode`/`modrm_reg` logic (this method reuses `modrm_reg` directly, passing the extension digit where a register encoding would normally go). If empirical disassembly disagrees with the golden bytes, investigate `alu_reg_imm` or `AluOp::extension()`, don't adjust the bytes.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `imul_reg_reg` + `imul_reg_reg_imm32`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

/// Direction check, part 1: dst=R9 (needs REX.R since it's in the reg
/// slot), src=Rax.
#[test]
fn imul_reg_reg_direction_dst_r9_src_rax() {
    let mut a = Assembler::new();
    a.imul_reg_reg(PhysReg::R9, PhysReg::Rax);
    assert_eq!(a.code(), &[0x4C, 0x0F, 0xAF, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["imul r9,rax"]);
}

/// Direction check, part 2: the operands from part 1 swapped (dst=Rax,
/// src=R9, needing REX.B instead of REX.R this time) -- together these two
/// tests prove imul_reg_reg's reg/rm assignment isn't accidentally
/// swapped, since a swap bug would make one of these two cases produce
/// the OTHER case's bytes instead of its own.
#[test]
fn imul_reg_reg_direction_dst_rax_src_r9() {
    let mut a = Assembler::new();
    a.imul_reg_reg(PhysReg::Rax, PhysReg::R9);
    assert_eq!(a.code(), &[0x49, 0x0F, 0xAF, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["imul rax,r9"]);
}

#[test]
fn imul_reg_reg_imm32_three_operand_form() {
    let mut a = Assembler::new();
    a.imul_reg_reg_imm32(PhysReg::Rax, PhysReg::Rbx, 10);
    assert_eq!(a.code(), &[0x48, 0x69, 0xC3, 0x0A, 0x00, 0x00, 0x00]);
    // Confirmed empirically: iced-x86 renders it in hex ("0Ah"), not
    // decimal ("10"), consistent with alu_reg_imm's earlier finding.
    assert_eq!(disassemble(a.code()), vec!["imul rax,rbx,0Ah"]);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `imul_reg_reg`/`imul_reg_reg_imm32` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
    /// `imul dst, src` -- dst *= src. REX.W + 0F AF /r. Unlike
    /// `alu_reg_reg`'s group-1 ops, this is a LOAD-direction opcode:
    /// ModRM.reg is the destination, ModRM.rm is the source. This isn't a
    /// design choice -- it's the only two-operand IMUL r64,r/m64 encoding
    /// x86-64 has. Do not copy `alu_reg_reg`'s reg/rm assignment here.
    pub fn imul_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0xAF);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `imul dst, src, imm` -- dst = src * imm (three-operand,
    /// non-destructive). REX.W + 69 /r id. Same reg=dst/rm=src direction
    /// as `imul_reg_reg` (consistent with itself, still opposite to
    /// group-1's convention).
    pub fn imul_reg_reg_imm32(&mut self, dst: PhysReg, src: PhysReg, imm: i32) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x69);
        self.modrm_reg(dst.encoding(), src.encoding());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 27 existing integration + 3 new = 30 integration). Note: Task 2's own review added 2 extra tests beyond its original plan estimate, so the real baseline here is 27, not 25 -- trust the actual running count from `cargo test`, not this plan's per-task arithmetic.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): imul_reg_reg + imul_reg_reg_imm32"
```

## Context for this task

`imul`'s reg/rm direction is opposite to every other method built so far in this file (`mov_reg_reg`, `alu_reg_reg` are both store-direction: rm=dest; `imul` is load-direction: reg=dest) — this is the single easiest place in this task to introduce a silent, plausible-looking bug, which is exactly why the two direction-check tests exist as a pair. If either fails, do not "fix" it by making `imul_reg_reg` match `alu_reg_reg`'s convention — that would be encoding a real x86-64 instruction incorrectly. Re-derive the encoding from the `0F AF /r` opcode reference instead.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: `mov_reg_imm`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn mov_reg_imm_uses_the_compact_form_when_the_value_fits_in_i32() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, 42);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC0, 0x2A, 0x00, 0x00, 0x00]);
    // Confirmed empirically: iced-x86 renders it in hex ("2Ah"), not
    // decimal ("42"), consistent with every other immediate/displacement
    // rendering found so far in Phases 6a/6b.
    assert_eq!(disassemble(a.code()), vec!["mov rax,2Ah"]);
}

#[test]
fn mov_reg_imm_compact_form_handles_a_negative_value() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rbx, -1);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC3, 0xFF, 0xFF, 0xFF, 0xFF]);
    // Confirmed empirically: iced-x86 renders the sign-extended 64-bit
    // value as its all-ones hex pattern, not decimal -1.
    assert_eq!(disassemble(a.code()), vec!["mov rbx,0FFFFFFFFFFFFFFFFh"]);
}

#[test]
fn mov_reg_imm_uses_movabs_for_a_value_that_does_not_fit_in_i32() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, i64::MAX);
    assert_eq!(
        a.code(),
        &[0x48, 0xB8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]
    );
    // Confirmed empirically: iced-x86's NasmFormatter names the B8+rd
    // (REX.W, imm64) form "mov", never "movabs" -- resolved, not just
    // guessed.
    assert_eq!(
        disassemble(a.code()),
        vec!["mov rax,7FFFFFFFFFFFFFFFh"]
    );
}

/// The movabs form has NO ModRM byte -- the destination register is
/// encoded directly into the opcode byte's low 3 bits, with REX.B (not
/// REX.R) covering extension. This test specifically confirms that still
/// works correctly for an extended register with no ModRM byte present to
/// normally carry that signal.
#[test]
fn mov_reg_imm_movabs_with_an_extended_register_still_sets_rex_b() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::R9, i64::MAX);
    assert_eq!(
        a.code(),
        &[0x49, 0xB9, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]
    );
    // Confirmed empirically: matches, same "mov"-not-"movabs" mnemonic and
    // hex rendering as the previous test.
    assert_eq!(disassemble(a.code()), vec!["mov r9,7FFFFFFFFFFFFFFFh"]);
}
```

**Resolved during implementation:** of the four originally-flagged unverified strings, two were wrong (the compact-form tests — hex-not-decimal rendering, consistent with every other immediate found so far) and two matched the guess (the movabs tests). This also resolved the mnemonic-naming uncertainty: iced-x86's `NasmFormatter` names the B8+rd/REX.W imm64 form `"mov"`, never `"movabs"`. All four strings above are now the empirically-confirmed values, not guesses.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `mov_reg_imm` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
    /// `mov dst, value` -- auto-selects the compact sign-extended-imm32
    /// form (REX.W + C7 /0 id) when `value` fits in i32, else the full
    /// 10-byte "movabs" form (REX.W + B8+rd io). The movabs form has NO
    /// ModRM byte at all -- the destination register is encoded directly
    /// into the low 3 bits of the opcode byte, with REX.B (not REX.R)
    /// covering register extension.
    pub fn mov_reg_imm(&mut self, dst: PhysReg, value: i64) {
        if let Ok(imm32) = i32::try_from(value) {
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xC7);
            self.modrm_reg(0, dst.encoding());
            self.code.extend_from_slice(&imm32.to_le_bytes());
        } else {
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xB8 + (dst.encoding() & 7));
            self.code.extend_from_slice(&value.to_le_bytes());
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 30 existing integration + 4 new = 34 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): mov_reg_imm with compact/movabs auto-selection"
```

## Context for this task

This is the first instruction in the crate with no ModRM byte at all — `rex()` is still called the same way (it doesn't know or care whether a ModRM byte follows), but the register-in-opcode-byte pattern (`0xB8 + (dst.encoding() & 7)`) is genuinely new. If `mov_reg_imm_movabs_with_an_extended_register_still_sets_rex_b` fails, check that `dst.encoding() & 7` is correctly masking to the low 3 bits (the same masking `modrm_reg`/`modrm_mem` already use for their reg/rm fields) rather than accidentally leaking the high bit into the opcode byte, which would produce a different, wrong opcode entirely rather than an obviously-broken one.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: `mov_mem_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn mov_mem_reg_generic_base_stores_correctly() {
    let mut a = Assembler::new();
    a.mov_mem_reg(PhysReg::Rcx, 8, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0x89, 0x41, 0x08]);
    // Confirms genuine STORE direction -- if reg/mem were accidentally
    // swapped with mov_reg_mem's LOAD semantics, this would disassemble
    // as "mov rax,[rcx+8]" instead.
    assert_eq!(disassemble(a.code()), vec!["mov [rcx+8],rax"]);
}

#[test]
fn mov_mem_reg_rsp_base_requires_sib() {
    let mut a = Assembler::new();
    a.mov_mem_reg(PhysReg::Rsp, 0, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0x89, 0x04, 0x24]);
    assert_eq!(disassemble(a.code()), vec!["mov [rsp],rax"]);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `mov_mem_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
    /// `mov [base + disp], src` -- 64-bit store. REX.W + 89 /r: the
    /// mirror image of `mov_reg_mem` (which uses 0x8B, load direction).
    /// Reuses `modrm_mem` directly, just swapping which operand is
    /// register vs. memory relative to `mov_reg_mem`'s call shape.
    pub fn mov_mem_reg(&mut self, base: PhysReg, disp: i32, src: PhysReg) {
        self.rex(true, src.encoding(), 0, base.encoding());
        self.code.push(0x89);
        self.modrm_mem(src.encoding(), base.encoding(), disp);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 38 existing integration + 2 new = 40 integration). Note: Task 4's own review added 4 more tests beyond its original plan estimate (pinning `mov_reg_imm`'s i32 boundary, commit `90ca4b4`), so the real baseline here is 38, not 34 -- trust the actual running count from `cargo test`, not this plan's per-task arithmetic.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): mov_mem_reg, the store direction mov_reg_mem was missing"
```

## Context for this task

This method is almost entirely a call-shape change on top of already-proven machinery (`modrm_mem` was exhaustively tested for all four rsp/rbp/r12/r13 special cases in 6a's Task 4 — those tests aren't repeated here since `modrm_mem`'s internal branching doesn't care whether it's called from a load or a store instruction, only `reg`/`base`/`disp`). The two tests here exist specifically to confirm the STORE direction is genuinely different from 6a's `mov_reg_mem` (LOAD direction) — the risk isn't in `modrm_mem`, it's in accidentally writing this method as a copy-paste of `mov_reg_mem` with the wrong opcode, or with `reg`/`rm` roles unintentionally matching the load form.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 6: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including all of `forge-x64`'s new tests. Report the exact final counts (they may differ slightly from this plan's running estimates if review rounds add tests, as happened repeatedly in 6a — trust the actual run, not this plan's numbers).

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Confirm no regressions in 6a's work or any other crate**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | tail -10` (confirm 6a's original tests — `mov_reg_reg`, `mov_reg_mem`, `jmp`, labels/fixups — are still present and passing alongside this plan's new tests) and `make spike` (confirm the Phase 0 day-one spike still works).

- [ ] **Step 5: Report exit criteria status**

Confirm all 6 exit criteria from the design doc are met:
1. `AluOp`, `alu_reg_reg`, `alu_reg_imm` exist and pass both golden-byte and disassembler-round-trip tests for all 5 operations, with imm8 and imm32 both exercised. ✅ (Tasks 1-2)
2. `imul_reg_reg` and `imul_reg_reg_imm32` exist and pass tests explicitly confirming operand direction. ✅ (Task 3)
3. `mov_reg_imm` exists, auto-selects correctly, and is tested for both the compact and movabs paths, including an extended-register movabs case. ✅ (Task 4)
4. `mov_mem_reg` exists and is tested to confirm genuine store-direction semantics via disassembly. ✅ (Task 5)
5. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 1-3)
6. No regressions in 6a's existing tests or any other crate's tests. ✅ (Step 4)
