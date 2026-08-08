# forge Phase 6c x86-64 Comparisons and Conditional Branches Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build comparisons and conditional branching in `forge-x64` — `cmp` (via a new `AluOp` variant), `test`, `setcc`, `cmovcc`, and `jcc` — so forge's `if`/`else` construct can actually compile, verified via the same golden-byte + `iced-x86` disassembler round-trip discipline established in Phases 6a/6b.

**Architecture:** All new methods live in `crates/forge-x64/src/assembler.rs`, built on 6a's/6b's existing `rex()`/`modrm_reg()`/`AluOp`/`Fixup`/`bind`/`jmp`-pattern machinery. A new `ConditionCode` enum (all 16 x86-64 condition codes) is shared across `setcc`/`cmovcc`/`jcc`'s opcode computation. `cmp` costs zero new encoder logic (just a new `AluOp` variant). `setcc` introduces one genuinely new mechanism: a REX-prefix-forcing rule for byte-sized destinations, to dodge x86's third REX trap (spl/bpl/sil/dil vs. ah/ch/dh/bh) the first time this crate ever writes a byte register. `jcc` reuses `jmp`'s rel8/rel32/`Fixup` machinery unmodified, adjusted only for its own different instruction lengths.

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only — already wired in Phase 6a, no Cargo.toml changes needed).

**Design doc:** `docs/superpowers/specs/2026-08-08-phase-6c-x64-comparisons-design.md` — read this first.

**A note on running test counts:** every task below states an "Expected" pass count computed from the prior task's estimate. In both 6a and 6b, review rounds repeatedly added extra tests beyond a task's original estimate, making later tasks' arithmetic stale — this was corrected after the fact each time. Treat every count in this plan as a best-effort estimate, not ground truth: always trust the actual output of `cargo test -p forge-x64`, and if a later task's baseline looks wrong, it's because an earlier task's review added tests — check `git log` for that task's commits rather than assuming the plan is right.

---

## Task 1: `AluOp::Cmp`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn alu_reg_reg_cmp() {
    let mut a = Assembler::new();
    a.alu_reg_reg(AluOp::Cmp, PhysReg::Rax, PhysReg::Rbx);
    assert_eq!(a.code(), &[0x48, 0x39, 0xD8]);
    assert_eq!(disassemble(a.code()), vec!["cmp rax,rbx"]);
}

#[test]
fn alu_reg_imm_cmp() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::Cmp, PhysReg::Rax, 5);
    assert_eq!(a.code(), &[0x48, 0x83, 0xF8, 0x05]);
    assert_eq!(disassemble(a.code()), vec!["cmp rax,5"]);
}
```

**IMPORTANT — before trusting the disassembly strings above**: both were hand-derived by direct analogy with already-verified 6b bytes (`cmp` follows the exact same group-1 pattern as `sub`/`and`, just a different opcode/extension digit), but per this project's established discipline, verify them empirically: run with the string assertions temporarily removed, confirm the golden bytes pass, then observe the actual `iced-x86` output and correct any string that doesn't match before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `AluOp::Cmp` variant doesn't exist.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — modify the AluOp enum and its two impl methods

pub enum AluOp {
    Add,
    Or,
    And,
    Sub,
    Xor,
    Cmp,
}

impl AluOp {
    fn extension(self) -> u8 {
        match self {
            AluOp::Add => 0,
            AluOp::Or => 1,
            AluOp::And => 4,
            AluOp::Sub => 5,
            AluOp::Xor => 6,
            AluOp::Cmp => 7,
        }
    }

    fn rr_opcode(self) -> u8 {
        match self {
            AluOp::Add => 0x01,
            AluOp::Or => 0x09,
            AluOp::And => 0x21,
            AluOp::Sub => 0x29,
            AluOp::Xor => 0x31,
            AluOp::Cmp => 0x39,
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib tests unchanged + 42 integration tests: 40 existing + 2 new).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): AluOp::Cmp"
```

## Context for this task

`cmp` computes `dst - src` (or `dst - imm`) and discards the result, keeping only the flags — architecturally it's group-1's `/7` extension digit with r/r opcode `0x39`, fitting the exact same pattern `AluOp::Sub` already uses. No changes to `alu_reg_reg`/`alu_reg_imm` themselves are needed; adding the enum variant and its two opcode-table entries is the entire task.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `test_reg_reg` + `test_reg_imm`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn test_reg_reg_self_test_is_the_zero_check_idiom() {
    let mut a = Assembler::new();
    a.test_reg_reg(PhysReg::Rax, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0x85, 0xC0]);
    assert_eq!(disassemble(a.code()), vec!["test rax,rax"]);
}

#[test]
fn test_reg_reg_with_extended_registers() {
    let mut a = Assembler::new();
    a.test_reg_reg(PhysReg::Rbx, PhysReg::R9);
    assert_eq!(a.code(), &[0x4C, 0x85, 0xCB]);
    assert_eq!(disassemble(a.code()), vec!["test rbx,r9"]);
}

#[test]
fn test_reg_imm_checks_a_bit_pattern() {
    let mut a = Assembler::new();
    a.test_reg_imm(PhysReg::Rax, 1000);
    assert_eq!(a.code(), &[0x48, 0xF7, 0xC0, 0xE8, 0x03, 0x00, 0x00]);
    // Confirmed empirically: iced-x86 renders it in hex ("3E8h"), not
    // decimal ("1000"), consistent with 6b's alu_reg_imm/mov_reg_imm findings.
    assert_eq!(disassemble(a.code()), vec!["test rax,3E8h"]);
}
```

**IMPORTANT**: verify all three disassembly strings empirically before committing — the third one in particular is flagged as likely wrong based on 6b's repeated findings. Don't guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `test_reg_reg`/`test_reg_imm` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
    /// `test a, b` -- computes `a & b`, discards the result, sets flags
    /// only. REX.W + 85 /r. Symmetric: unlike mov/alu's store-direction
    /// convention, there's no meaningful "which operand is destination"
    /// distinction to get backward, since neither operand is written.
    pub fn test_reg_reg(&mut self, a: PhysReg, b: PhysReg) {
        self.rex(true, b.encoding(), 0, a.encoding());
        self.code.push(0x85);
        self.modrm_reg(b.encoding(), a.encoding());
    }

    /// `test dst, imm` -- REX.W + F7 /0 id. A completely separate opcode
    /// from group-1's 0x81/0x83 -- no imm8 form exists for `test` in
    /// real x86-64.
    pub fn test_reg_imm(&mut self, dst: PhysReg, imm: i32) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(0, dst.encoding());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 42 existing integration + 3 new = 45 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): test_reg_reg + test_reg_imm"
```

## Context for this task

`test_reg_imm` reuses `modrm_reg(0, dst.encoding())` the same way `alu_reg_imm` does for its extension digit — `0xF7 /0` is the group-3 opcode family's `TEST` selector (the same family that has `/2`=NOT, `/3`=NEG, `/4`=MUL, `/5`=IMUL, `/6`=DIV, `/7`=IDIV, none of which are in scope for this task).

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `ConditionCode` + `setcc`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

use forge_x64::ConditionCode;

#[test]
fn setcc_low_register_needs_no_rex() {
    let mut a = Assembler::new();
    a.setcc(ConditionCode::Equal, PhysReg::Rax);
    assert_eq!(a.code(), &[0x0F, 0x94, 0xC0]);
    // NOTE: verify this string empirically -- iced-x86's exact mnemonic
    // naming for this condition code (e.g. "sete" vs "setz") was not
    // checked against a live compile when this plan was written.
    assert_eq!(disassemble(a.code()), vec!["sete al"]);
}

/// THE critical test: rsp/rbp/rsi/rdi (encoding 4-7) as a setcc
/// destination need a REX prefix FORCED, even though nothing else about
/// this instruction would otherwise need one, to select spl/bpl/sil/dil
/// instead of ah/ch/dh/bh. If `rex_for_byte_dst` is missing or wrong,
/// this test's golden bytes stay the same length either way (0x40 is a
/// single byte) but the DISASSEMBLED NAME changes -- this is exactly the
/// kind of bug that produces a plausible-looking, silently wrong
/// encoding, per this project's repeated warnings about REX traps.
#[test]
fn setcc_rsp_encoding_forces_rex_to_avoid_ah_ch_dh_bh() {
    let mut a = Assembler::new();
    a.setcc(ConditionCode::NotEqual, PhysReg::Rsp);
    assert_eq!(a.code(), &[0x40, 0x0F, 0x95, 0xC4]);
    // NOTE: verify this string empirically. If the REX-forcing logic is
    // broken, the bytes might still be [0x0F, 0x95, 0xC4] (no 0x40) and
    // this assertion would need to change to "setne ah" instead -- if
    // that happens, the bug is in rex_for_byte_dst, not in this test.
    assert_eq!(disassemble(a.code()), vec!["setne spl"]);
}

#[test]
fn setcc_extended_register_already_forces_rex() {
    let mut a = Assembler::new();
    a.setcc(ConditionCode::Less, PhysReg::R9);
    assert_eq!(a.code(), &[0x41, 0x0F, 0x9C, 0xC1]);
    // NOTE: verify this string empirically.
    assert_eq!(disassemble(a.code()), vec!["setl r9b"]);
}
```

**CRITICAL**: verify all three disassembly strings empirically, but pay special attention to `setcc_rsp_encoding_forces_rex_to_avoid_ah_ch_dh_bh` — this is the one test in this whole plan specifically designed to catch a REX-trap bug, and if it fails, the fix belongs in `rex_for_byte_dst`, not in loosening this test.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `ConditionCode`/`setcc` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add near AluOp, above the `#[cfg(test)]` module

/// One of x86-64's 16 condition codes, usable with `setcc`/`cmovcc`/`jcc`.
/// forge's current i64 comparisons only need Equal/NotEqual/Less/
/// GreaterOrEqual/LessOrEqual/Greater, but all 16 are implemented now --
/// the other 10 (unsigned, sign, overflow, parity) will matter once
/// forge grows unsigned or float comparisons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConditionCode {
    Overflow,
    NotOverflow,
    Below,
    AboveOrEqual,
    Equal,
    NotEqual,
    BelowOrEqual,
    Above,
    Sign,
    NotSign,
    Parity,
    NotParity,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    Greater,
}

impl ConditionCode {
    /// The 4-bit "cc" nibble -- the low 4 bits of the corresponding
    /// Jcc/SETcc/CMOVcc opcode byte, matching Intel's canonical ordering.
    fn nibble(self) -> u8 {
        use ConditionCode::*;
        match self {
            Overflow => 0,
            NotOverflow => 1,
            Below => 2,
            AboveOrEqual => 3,
            Equal => 4,
            NotEqual => 5,
            BelowOrEqual => 6,
            Above => 7,
            Sign => 8,
            NotSign => 9,
            Parity => 10,
            NotParity => 11,
            Less => 12,
            GreaterOrEqual => 13,
            LessOrEqual => 14,
            Greater => 15,
        }
    }
}

impl Assembler {
    /// Forces a REX prefix (even a "no-op" 0x40) when `dst` is in the
    /// 4-7 encoding range, to select spl/bpl/sil/dil instead of
    /// ah/ch/dh/bh for byte-sized operations -- the third REX trap
    /// SPEC.md warns about, and the first place in this crate that
    /// writes a byte-sized destination. Encodings 0-3 are unambiguous
    /// either way (al/cl/dl/bl); encodings 8-15 already force a REX
    /// prefix via their extension bit through the normal rex() path.
    fn rex_for_byte_dst(&mut self, dst: u8) {
        if (4..=7).contains(&dst) {
            self.code.push(0x40);
        } else {
            self.rex(false, 0, 0, dst);
        }
    }

    /// `setcc dst` -- writes 0 or 1 to the low byte of `dst` based on
    /// `cc`. 0F 90+cc /0. Writes ONLY one byte -- the upper bits of
    /// `dst` are left as whatever was there before; producing a clean
    /// full-width 0/1 is instruction-selection's job (e.g. an xor
    /// before this call), not this method's.
    pub fn setcc(&mut self, cc: ConditionCode, dst: PhysReg) {
        self.rex_for_byte_dst(dst.encoding());
        self.code.push(0x0F);
        self.code.push(0x90 + cc.nibble());
        self.modrm_reg(0, dst.encoding());
    }
}
```

- [ ] **Step 4: Export `ConditionCode` from `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 45 existing integration + 3 new = 48 integration).

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): ConditionCode + setcc, with the byte-register REX trap handled"
```

## Context for this task

This is the highest-risk task in this plan — the first time this crate writes a byte-sized register, and exactly the kind of REX subtlety this project's docs repeatedly warn about (SPEC.md's own rex() doc comment already names this exact trap: "With ANY REX prefix, byte registers spl/bpl/sil/dil replace ah/ch/dh/bh — silently different registers"). If `setcc_rsp_encoding_forces_rex_to_avoid_ah_ch_dh_bh` fails, the golden bytes tell you everything: `[0x40, 0x0F, 0x95, 0xC4]` (4 bytes, REX forced) is correct; `[0x0F, 0x95, 0xC4]` (3 bytes, no REX) means `rex_for_byte_dst` isn't forcing the prefix for encoding 4-7 and needs fixing, not the test relaxing.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: `cmovcc`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

/// Direction check, part 1: dst=R9 (needs REX.R), src=Rax.
#[test]
fn cmovcc_direction_dst_r9_src_rax() {
    let mut a = Assembler::new();
    a.cmovcc(ConditionCode::Greater, PhysReg::R9, PhysReg::Rax);
    assert_eq!(a.code(), &[0x4C, 0x0F, 0x4F, 0xC8]);
    // NOTE: verify this string empirically -- iced-x86's mnemonic naming
    // for Greater (e.g. "cmovg" vs "cmovnle") was not checked against a
    // live compile when this plan was written.
    assert_eq!(disassemble(a.code()), vec!["cmovg r9,rax"]);
}

/// Direction check, part 2: the operands from part 1 swapped -- together
/// these two tests prove cmovcc's reg/rm assignment isn't accidentally
/// swapped, mirroring imul_reg_reg's direction-check pair from 6b.
#[test]
fn cmovcc_direction_dst_rax_src_r9() {
    let mut a = Assembler::new();
    a.cmovcc(ConditionCode::Greater, PhysReg::Rax, PhysReg::R9);
    assert_eq!(a.code(), &[0x49, 0x0F, 0x4F, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["cmovg rax,r9"]);
}
```

**IMPORTANT**: verify both disassembly strings empirically before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `cmovcc` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
    /// `cmovcc dst, src` -- dst = src if cc holds, else dst unchanged.
    /// REX.W + 0F 40+cc /r. Load-direction (reg=dst, rm=src), same
    /// convention as imul_reg_reg. No byte-register concern -- x86 has
    /// no 8-bit CMOVcc.
    pub fn cmovcc(&mut self, cc: ConditionCode, dst: PhysReg, src: PhysReg) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x40 + cc.nibble());
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 48 existing integration + 2 new = 50 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): cmovcc"
```

## Context for this task

`cmovcc` follows `imul_reg_reg`'s load-direction convention exactly (reg=dst, rm=src) — the opposite of `alu_reg_reg`/`test_reg_reg`'s store-direction convention. The two direction-check tests mirror `imul_reg_reg`'s pair from 6b for the same reason: if either fails, the bug is almost certainly a reg/rm swap in `cmovcc` itself, not something to fix by making it match `alu_reg_reg`'s convention.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: `jcc`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn jcc_backward_short_uses_rel8() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler: 48 89 C0
    let len_before_jcc = a.code().len();
    a.jcc(ConditionCode::Equal, l); // backward reference, close enough for rel8

    let expected_rel = -(len_before_jcc as i32 + 2); // rel8 measured from the end of this 2-byte instruction
    assert_eq!(a.code()[len_before_jcc], 0x70 + 4); // Equal's nibble is 4
    assert_eq!(a.code()[len_before_jcc + 1], expected_rel as i8 as u8);
    assert_eq!(a.code().len(), len_before_jcc + 2);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with('j'));
}

#[test]
fn jcc_backward_near_uses_a_six_byte_form() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    for _ in 0..50 {
        a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes each, 150 bytes total -- far enough that rel8 can't reach
    }
    let len_before_jcc = a.code().len();
    a.jcc(ConditionCode::NotEqual, l); // backward reference, too far for rel8

    // jcc's near form is 6 bytes (0F 80+cc + rel32), one byte longer than
    // jmp's 5-byte near form (E9 + rel32), since the conditional opcode
    // is 2 bytes, not 1.
    let expected_rel = -(len_before_jcc as i32 + 6);
    assert_eq!(a.code()[len_before_jcc], 0x0F);
    assert_eq!(a.code()[len_before_jcc + 1], 0x80 + 5); // NotEqual's nibble is 5
    assert_eq!(
        &a.code()[len_before_jcc + 2..len_before_jcc + 6],
        &expected_rel.to_le_bytes()
    );
    assert_eq!(a.code().len(), len_before_jcc + 6);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with('j'));
}

#[test]
fn jcc_forward_always_uses_the_near_form() {
    let mut a = Assembler::new();
    let l = a.new_label();
    let jcc_at = a.code().len(); // 0
    a.jcc(ConditionCode::Less, l); // forward reference -- label not bound yet
    assert_eq!(a.code()[jcc_at], 0x0F);
    assert_eq!(a.code()[jcc_at + 1], 0x80 + 12); // Less's nibble is 12
    assert_eq!(a.code().len(), jcc_at + 6); // always the 6-byte near form for forward jumps, never the 2-byte short form

    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler between the jcc and its target
    let target_pos = a.code().len();
    a.bind(l); // resolves the fixup recorded above

    let expected_rel = target_pos as i32 - (jcc_at as i32 + 6);
    assert_eq!(&a.code()[jcc_at + 2..jcc_at + 6], &expected_rel.to_le_bytes());

    let text = disassemble(a.code());
    assert!(text[0].starts_with('j'));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `jcc` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `jmp`

impl Assembler {
    /// `jcc label` -- conditional jump. Mirrors jmp's rel8/rel32
    /// auto-selection and Fixup reuse exactly, except for length: the
    /// short form is 2 bytes (70+cc, rel8), the near form is 6 bytes
    /// (0F 80+cc, rel32) -- one byte longer than jmp's 5-byte near form,
    /// since the conditional opcode is 2 bytes, not 1. patch_fixup()
    /// needs no changes: it only depends on fixup.at (the position of
    /// the 4 placeholder bytes), not on how long the preceding opcode
    /// was.
    pub fn jcc(&mut self, cc: ConditionCode, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end_if_short = self.code.len() + 2;
            let rel = target_pos as isize - end_if_short as isize;
            if let Ok(rel8) = i8::try_from(rel) {
                self.code.push(0x70 + cc.nibble());
                self.code.push(rel8 as u8);
            } else {
                let end_if_near = self.code.len() + 6;
                let rel32 = target_pos as isize - end_if_near as isize;
                self.code.push(0x0F);
                self.code.push(0x80 + cc.nibble());
                self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
            }
        } else {
            self.code.push(0x0F);
            self.code.push(0x80 + cc.nibble());
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]); // placeholder, patched by bind()
            self.fixups.push(Fixup { at, target: label });
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 50 existing integration + 3 new = 53 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): jcc, reusing jmp's Fixup machinery with its own instruction lengths"
```

## Context for this task

The three test cases deliberately express expected relative-offset bytes as a formula computed from `a.code().len()` at each observation point, exactly matching 6a's `jmp` tests' approach — this is more robust than hand-derived magic numbers and is itself the target-address verification the design doc calls for. `jcc` reuses `Fixup`/`bind`/`patch_fixup` completely unmodified (no changes to those functions in this task) — only `jcc`'s own forward/backward branches differ from `jmp`'s, and only in byte-length arithmetic (`+2`/`+6` here vs. `jmp`'s `+2`/`+5`). If a test fails, check the length arithmetic first (did you use `+5` by copy-pasting from `jmp` instead of `+6`?) before suspecting `Fixup`/`bind`.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 6: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including all of `forge-x64`'s new tests. Report the exact final counts — per this plan's header note, trust the actual run over the plan's per-task arithmetic.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Confirm no regressions in 6a's/6b's work**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | tail -10` (confirm 6a's `mov_reg_reg`/`mov_reg_mem`/`jmp`/label-fixup tests and 6b's `alu_reg_reg`/`alu_reg_imm`/`imul_*`/`mov_reg_imm`/`mov_mem_reg` tests are all still present and passing alongside 6c's new tests) and `make spike` (confirm the Phase 0 day-one spike still works).

- [ ] **Step 5: Report exit criteria status**

Confirm all 7 exit criteria from the design doc are met:
1. `AluOp::Cmp` exists and passes r/r and r/imm tests via the existing machinery. ✅ (Task 1)
2. `test_reg_reg` and `test_reg_imm` exist and pass tests, including the zero-check idiom. ✅ (Task 2)
3. `ConditionCode` (all 16 variants) and `setcc` exist; the byte-register REX-forcing rule is tested for encodings 0-3, 4-7, and 8-15 specifically. ✅ (Task 3)
4. `cmovcc` exists and passes a direction-check test. ✅ (Task 4)
5. `jcc` exists and passes backward-short, backward-near, and forward-near tests, with the 6-byte near-form length explicitly confirmed. ✅ (Task 5)
6. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 1-3)
7. No regressions in 6a's/6b's existing tests or any other crate's tests. ✅ (Step 4)
