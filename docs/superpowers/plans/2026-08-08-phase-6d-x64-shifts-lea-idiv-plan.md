# forge Phase 6d x86-64 Shifts, Unary Ops, LEA, and 128-bit Division Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `neg`/`not`/`inc`/`dec`, `shl`/`shr`/`sar` (imm8 and CL forms), `lea` (including the 3-operand scaled-index form), and the 128-bit `imul` form plus `idiv`/`cqo` in `forge-x64` — the last of which unlocks Phase 4's already-built magic-number division strength reduction — verified via the same golden-byte + `iced-x86` disassembler round-trip discipline established in Phases 6a-6c.

**Architecture:** All new methods live in `crates/forge-x64/src/assembler.rs`, built on 6a-6c's existing `rex()`/`modrm_reg()`/`modrm_mem()`/`DispMode` machinery. Four unary methods stay standalone (they split across two unrelated opcodes, no shared structure to abstract). A new `ShiftOp` enum (`Shl`/`Shr`/`Sar`) mirrors `AluOp`'s justification — genuinely shared structure across 3 operations. `lea_reg_scaled` is the substantial new piece: the first real SIB-with-index encoding in this crate, and the first real exercise of `rex()`'s `index` parameter.

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only — already wired in Phase 6a, no Cargo.toml changes needed).

**Design doc:** `docs/superpowers/specs/2026-08-08-phase-6d-x64-shifts-lea-idiv-design.md` — read this first.

**A note on running test counts:** every task below states an "Expected" pass count computed from the prior task's estimate, starting from the confirmed baseline of 14 lib + 53 integration tests at the end of Phase 6c. In every prior Phase 6 sub-slice, review rounds sometimes added extra tests beyond a task's original estimate, making later tasks' arithmetic stale. Treat every count in this plan as a best-effort estimate, not ground truth: always trust the actual output of `cargo test -p forge-x64`, and if a later task's baseline looks wrong, check `git log` for the actual test count in the prior task's final commit rather than assuming this plan's running arithmetic is right.

---

## Task 1: `not_reg` / `neg_reg` / `inc_reg` / `dec_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn not_reg_flips_all_bits() {
    let mut a = Assembler::new();
    a.not_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0xF7, 0xD0]);
    assert_eq!(disassemble(a.code()), vec!["not rax"]);
}

#[test]
fn neg_reg_negates() {
    let mut a = Assembler::new();
    a.neg_reg(PhysReg::Rbx);
    assert_eq!(a.code(), &[0x48, 0xF7, 0xDB]);
    assert_eq!(disassemble(a.code()), vec!["neg rbx"]);
}

#[test]
fn inc_reg_uses_the_modrm_form_not_a_rex_conflicting_opcode() {
    let mut a = Assembler::new();
    a.inc_reg(PhysReg::R9);
    assert_eq!(a.code(), &[0x49, 0xFF, 0xC1]);
    assert_eq!(disassemble(a.code()), vec!["inc r9"]);
}

#[test]
fn dec_reg_uses_the_modrm_form() {
    let mut a = Assembler::new();
    a.dec_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0xFF, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["dec rax"]);
}
```

**IMPORTANT — before trusting the disassembly strings above**: they were hand-derived from the standard x86-64 group-3/group-5 opcode tables, but per this project's established discipline, verify them empirically: run with the string assertions temporarily removed, confirm the golden bytes pass, then observe the actual `iced-x86` output and correct any string that doesn't match before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `not_reg`/`neg_reg`/`inc_reg`/`dec_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `test_reg_reg`

impl Assembler {
    /// `not dst` -- REX.W + F7 /2, one's complement in place.
    pub fn not_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(2, dst.encoding());
    }

    /// `neg dst` -- REX.W + F7 /3, two's complement negation in place.
    pub fn neg_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(3, dst.encoding());
    }

    /// `inc dst` -- REX.W + FF /0. In 64-bit mode the old single-byte
    /// INC opcodes (0x40-0x47) were repurposed as REX prefixes, so this
    /// ModRM-based form (group 5) is the only encoding that exists.
    pub fn inc_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xFF);
        self.modrm_reg(0, dst.encoding());
    }

    /// `dec dst` -- REX.W + FF /1. Same 64-bit-mode note as inc_reg.
    pub fn dec_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xFF);
        self.modrm_reg(1, dst.encoding());
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib tests unchanged + 57 integration tests: 53 existing + 4 new).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): not_reg, neg_reg, inc_reg, dec_reg"
```

## Context for this task

`not`/`neg` share opcode `0xF7` (group 3, the same family `test_reg_imm` already uses for its `/0` extension); `inc`/`dec` use the unrelated `0xFF` (group 5) because 64-bit mode repurposed the classic single-byte INC/DEC opcodes as REX prefixes. This is why the design doc deliberately does NOT introduce a shared enum here — four standalone methods reflect the real opcode split more honestly than forcing all four into one abstraction would.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `ShiftOp` + `shift_reg_imm8` / `shift_reg_cl`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

use forge_x64::ShiftOp;

#[test]
fn shift_reg_imm8_shl() {
    let mut a = Assembler::new();
    a.shift_reg_imm8(ShiftOp::Shl, PhysReg::Rax, 3);
    assert_eq!(a.code(), &[0x48, 0xC1, 0xE0, 0x03]);
    assert_eq!(disassemble(a.code()), vec!["shl rax,3"]);
}

#[test]
fn shift_reg_imm8_shr() {
    let mut a = Assembler::new();
    a.shift_reg_imm8(ShiftOp::Shr, PhysReg::Rbx, 5);
    assert_eq!(a.code(), &[0x48, 0xC1, 0xEB, 0x05]);
    assert_eq!(disassemble(a.code()), vec!["shr rbx,5"]);
}

#[test]
fn shift_reg_imm8_sar() {
    let mut a = Assembler::new();
    a.shift_reg_imm8(ShiftOp::Sar, PhysReg::R9, 1);
    assert_eq!(a.code(), &[0x49, 0xC1, 0xF9, 0x01]);
    assert_eq!(disassemble(a.code()), vec!["sar r9,1"]);
}

#[test]
fn shift_reg_cl_takes_the_count_from_cl() {
    let mut a = Assembler::new();
    a.shift_reg_cl(ShiftOp::Shl, PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0xD3, 0xE0]);
    assert_eq!(disassemble(a.code()), vec!["shl rax,cl"]);
}
```

**IMPORTANT**: verify all four disassembly strings empirically before committing — don't guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `ShiftOp`/`shift_reg_imm8`/`shift_reg_cl` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add near AluOp/ConditionCode, above the `#[cfg(test)]` module

/// A group-2 shift operation: `shl`/`shr`/`sar` share the same opcode
/// pair (C1 /n for the immediate-shift-amount form, D3 /n for the
/// CL-count form), differing only by the ModRM.reg extension digit --
/// the same justification `AluOp` has for existing. /0-3 (rotate) and /6
/// (an unused alias for /4) aren't part of this crate's instruction set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShiftOp {
    Shl,
    Shr,
    Sar,
}

impl ShiftOp {
    fn extension(self) -> u8 {
        match self {
            ShiftOp::Shl => 4,
            ShiftOp::Shr => 5,
            ShiftOp::Sar => 7,
        }
    }
}

impl Assembler {
    /// `op dst, shift` -- REX.W + C1 /n ib. NOTE: x86 masks the shift
    /// count to the low 6 bits for 64-bit operands at EXECUTION time --
    /// this method does not mask `shift` itself, it encodes whatever
    /// byte it's given. A caller passing 64 encodes literally 64, which
    /// the CPU then treats as a shift by 0 at runtime, not as "shift out
    /// everything."
    pub fn shift_reg_imm8(&mut self, op: ShiftOp, dst: PhysReg, shift: u8) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xC1);
        self.modrm_reg(op.extension(), dst.encoding());
        self.code.push(shift);
    }

    /// `op dst, cl` -- REX.W + D3 /n. Shift count comes from CL (the low
    /// byte of RCX); the caller is responsible for having loaded the
    /// count into CL beforehand.
    pub fn shift_reg_cl(&mut self, op: ShiftOp, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xD3);
        self.modrm_reg(op.extension(), dst.encoding());
    }
}
```

- [ ] **Step 4: Export `ShiftOp` from `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, ShiftOp};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 58 existing integration + 4 new = 62 integration). Note: Task 1's own review added 1 extra test beyond its original plan estimate, so the real baseline here is 58, not 57 -- trust the actual running count from `cargo test`, not this plan's per-task arithmetic.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): ShiftOp + shift_reg_imm8/shift_reg_cl"
```

## Context for this task

Unlike the imm8/imm32/rel8/rel32/compact/movabs auto-selections built throughout 6a-6c, there is deliberately NO auto-selection of the 1-byte-shorter `D1 /n` shift-by-1 special form here — that's a pure code-size optimization with no correctness implication, and this JIT isn't optimizing for code size anywhere yet. `shift_reg_imm8`'s test for `Sar` uses a shift amount of exactly 1 specifically to confirm this: the golden bytes include the immediate byte `0x01`, proving the general `C1` form is used even for the case where the special-cased `D1` form would apply, not silently substituted.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `lea_reg_mem`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn lea_reg_mem_computes_an_address_not_a_dereference() {
    let mut a = Assembler::new();
    a.lea_reg_mem(PhysReg::Rax, PhysReg::Rcx, 8);
    assert_eq!(a.code(), &[0x48, 0x8D, 0x41, 0x08]);
    // Confirms genuinely `lea`, not `mov` -- if the opcode were
    // accidentally 0x8B (mov_reg_mem's load opcode) instead of 0x8D,
    // the bytes would differ by exactly one byte and this string would
    // read "mov rax,[rcx+8]" instead.
    assert_eq!(disassemble(a.code()), vec!["lea rax,[rcx+8]"]);
}
```

**IMPORTANT**: verify this disassembly string empirically before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `lea_reg_mem` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `mov_reg_mem`/`mov_mem_reg`

impl Assembler {
    /// `lea dst, [base + disp]` -- REX.W + 8D /r. Computes an address
    /// without dereferencing it. Reuses modrm_mem exactly like
    /// mov_reg_mem does, just with opcode 0x8D.
    pub fn lea_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        self.rex(true, dst.encoding(), 0, base.encoding());
        self.code.push(0x8D);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 62 existing integration + 1 new = 63 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): lea_reg_mem"
```

## Context for this task

This is deliberately the trivial half of `lea` — it reuses `modrm_mem` exactly as `mov_reg_mem` does, with no new addressing-mode logic. All four of `modrm_mem`'s special cases (rsp/rbp/r12/r13) were already exhaustively tested back in Phase 6a and don't need re-testing here for the same reason `mov_mem_reg` didn't re-test them in 6b: `modrm_mem`'s branching depends only on `base`/`disp`, never on which opcode called it. The genuinely new, higher-risk half of `lea` (`lea_reg_scaled`) is Task 4, not this one.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: `lea_reg_scaled`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn lea_reg_scaled_encodes_a_real_sib_index() {
    let mut a = Assembler::new();
    a.lea_reg_scaled(PhysReg::Rax, PhysReg::Rax, PhysReg::Rbx, 4, 0);
    assert_eq!(a.code(), &[0x48, 0x8D, 0x04, 0x98]);
    // NOTE: verify this string empirically -- this is the first real
    // scaled-index disassembly in this crate, not checked against a
    // live compile when this plan was written.
    assert_eq!(disassemble(a.code()), vec!["lea rax,[rax+rbx*4]"]);
}

/// RSP cannot be a scaled-index register -- x86 reserves SIB.index=100
/// to mean "no index," so this combination is architecturally
/// unencodable. Must panic loudly, not silently emit a wrong encoding.
#[test]
#[should_panic(expected = "RSP cannot be used as a scaled-index register")]
fn lea_reg_scaled_panics_when_index_is_rsp() {
    let mut a = Assembler::new();
    a.lea_reg_scaled(PhysReg::Rax, PhysReg::Rax, PhysReg::Rsp, 1, 0);
}

/// The rbp/r13-base-disp0-forces-disp8 trap (established in 6a's
/// modrm_mem) must still apply even when a real SIB index/scale is
/// also present -- these are two independent rules that both act on
/// the same instruction.
#[test]
fn lea_reg_scaled_rbp_base_with_zero_disp_still_forces_disp8() {
    let mut a = Assembler::new();
    a.lea_reg_scaled(PhysReg::Rax, PhysReg::Rbp, PhysReg::Rax, 2, 0);
    assert_eq!(a.code(), &[0x48, 0x8D, 0x44, 0x45, 0x00]);
    // NOTE: verify this string empirically.
    assert_eq!(disassemble(a.code()), vec!["lea rax,[rbp+rax*2]"]);
}
```

**CRITICAL**: this is the highest-risk task in this plan. Verify both non-panic disassembly strings empirically before committing — don't guess. If `lea_reg_scaled_panics_when_index_is_rsp` fails to panic, the bug is a missing/wrong assert in `lea_reg_scaled`, not something to work around.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `lea_reg_scaled` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the same `impl Assembler` block as `lea_reg_mem`

impl Assembler {
    /// `lea dst, [base + index*scale + disp]` -- REX.W + 8D /r with a
    /// real SIB byte (scale/index/base, not the "no index" pattern
    /// modrm_mem always emits for the rsp/r12 special case). First real
    /// use of rex()'s `index` parameter -- REX.X has been plumbed
    /// through since Phase 6a but never actually exercised until now.
    ///
    /// Two traps:
    ///   * `index == RSP` is architecturally unencodable: SIB.index=100
    ///     always means "no index," so there's no way to name RSP as a
    ///     scaled-index register. Asserted against.
    ///   * `base` in the rbp/r13 family (encoding low bits == 101) with
    ///     disp==0 still means RIP-relative unless forced to disp8, the
    ///     exact same trap modrm_mem handles for the base+disp-only
    ///     case -- it reapplies here identically, now combined with a
    ///     real index/scale in the SIB byte.
    pub fn lea_reg_scaled(
        &mut self,
        dst: PhysReg,
        base: PhysReg,
        index: PhysReg,
        scale: u8,
        disp: i32,
    ) {
        assert_ne!(
            index,
            PhysReg::Rsp,
            "RSP cannot be used as a scaled-index register -- x86 reserves \
             SIB.index=100 to mean \"no index\", so this combination is \
             architecturally unencodable"
        );
        let scale_bits = match scale {
            1 => 0b00,
            2 => 0b01,
            4 => 0b10,
            8 => 0b11,
            _ => panic!("scale must be 1, 2, 4, or 8, got {scale}"),
        };
        self.rex(true, dst.encoding(), index.encoding(), base.encoding());
        self.code.push(0x8D);
        let base_low = base.encoding() & 7;
        if base_low == 5 && disp == 0 {
            self.code.push(0b01 << 6 | ((dst.encoding() & 7) << 3) | 0b100);
            self.code
                .push(scale_bits << 6 | ((index.encoding() & 7) << 3) | base_low);
            self.code.push(0);
        } else {
            let mode = disp_mode(disp);
            self.code.push(mode.bits() << 6 | ((dst.encoding() & 7) << 3) | 0b100);
            self.code
                .push(scale_bits << 6 | ((index.encoding() & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 63 existing integration + 3 new = 66 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): lea_reg_scaled, the first real SIB-index encoding in this crate"
```

## Context for this task

The three test cases were hand-derived and cross-checked against the standard SIB-byte layout (`ss iii bbb`: scale bits, index register, base register) and against `modrm_mem`'s already-proven rbp/r13 trap logic. If `lea_reg_scaled_encodes_a_real_sib_index` disagrees with the golden bytes, check the SIB byte construction first (`scale_bits << 6 | ((index.encoding() & 7) << 3) | base_low` — a swapped shift amount here would silently produce a plausible-looking but wrong SIB byte). If `lea_reg_scaled_rbp_base_with_zero_disp_still_forces_disp8` fails, check that the `base_low == 5 && disp == 0` branch is still reached correctly with a real index present — it's easy to accidentally special-case only the "no index" path and forget this trap needs to fire regardless of what's in the SIB's index field.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: `imul128_reg` / `idiv_reg` / `cqo`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn imul128_reg_encodes_the_one_operand_form() {
    let mut a = Assembler::new();
    a.imul128_reg(PhysReg::Rbx);
    assert_eq!(a.code(), &[0x48, 0xF7, 0xEB]);
    assert_eq!(disassemble(a.code()), vec!["imul rbx"]);
}

#[test]
fn idiv_reg_encodes_the_divisor_operand() {
    let mut a = Assembler::new();
    a.idiv_reg(PhysReg::R9);
    assert_eq!(a.code(), &[0x49, 0xF7, 0xF9]);
    assert_eq!(disassemble(a.code()), vec!["idiv r9"]);
}

#[test]
fn cqo_sign_extends_rax_into_rdx_rax() {
    let mut a = Assembler::new();
    a.cqo();
    assert_eq!(a.code(), &[0x48, 0x99]);
    assert_eq!(disassemble(a.code()), vec!["cqo"]);
}
```

**IMPORTANT**: verify all three disassembly strings empirically before committing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `imul128_reg`/`idiv_reg`/`cqo` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `not_reg`/`neg_reg`

impl Assembler {
    /// `imul src` (one-operand form) -- RDX:RAX = RAX * src, signed,
    /// full 128-bit product. REX.W + F7 /5. Unlike every ModRM-based
    /// instruction so far, this has no explicit destination -- the
    /// result always lands in the implicit RDX:RAX pair.
    pub fn imul128_reg(&mut self, src: PhysReg) {
        self.rex(true, 0, 0, src.encoding());
        self.code.push(0xF7);
        self.modrm_reg(5, src.encoding());
    }

    /// `idiv src` -- RAX = RDX:RAX / src, RDX = RDX:RAX % src, signed.
    /// REX.W + F7 /7. PRECONDITION: RDX must already be the correct
    /// sign-extension of RAX (call cqo() first) -- otherwise this
    /// computes garbage or traps with #DE, even for divisions that
    /// don't actually overflow.
    pub fn idiv_reg(&mut self, src: PhysReg) {
        self.rex(true, 0, 0, src.encoding());
        self.code.push(0xF7);
        self.modrm_reg(7, src.encoding());
    }

    /// Sign-extends RAX into RDX:RAX -- the required precondition for
    /// idiv_reg. REX.W + 0x99. No ModRM, no operands -- the simplest
    /// instruction in this crate.
    pub fn cqo(&mut self) {
        self.rex(true, 0, 0, 0);
        self.code.push(0x99);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 69 existing integration + 3 new = 72 integration). Note: Task 4's own review added 3 extra tests beyond its original plan estimate, so the real baseline here is 69, not 66 -- trust the actual running count from `cargo test`, not this plan's per-task arithmetic.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): imul128_reg, idiv_reg, cqo"
```

## Context for this task

There is no way to test the *implicit* RDX:RAX semantics of `imul128_reg`/`idiv_reg` via disassembly text beyond confirming the mnemonic and single explicit operand — that limitation is inherent to what these instructions are (the disassembler shows you the encoding, not the runtime register-file effect), not a gap in this task's test coverage. `idiv_reg`'s doc comment states its RDX-sign-extension precondition explicitly; enforcing it at runtime (e.g. always emitting `cqo` before `idiv_reg`) is instruction-selection's job in a later phase, not this encoder method's.

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

- [ ] **Step 4: Confirm no regressions in 6a's/6b's/6c's work**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | tail -10` (confirm 6a's `mov_reg_reg`/`mov_reg_mem`/`jmp`/label-fixup tests, 6b's `alu_reg_reg`/`alu_reg_imm`/`imul_*`/`mov_reg_imm`/`mov_mem_reg` tests, and 6c's `alu_*_cmp`/`test_reg_*`/`setcc_*`/`cmovcc_*`/`jcc_*` tests are all still present and passing alongside 6d's new tests) and `make spike` (confirm the Phase 0 day-one spike still works).

- [ ] **Step 5: Report exit criteria status**

Confirm all 7 exit criteria from the design doc are met:
1. `not_reg`/`neg_reg`/`inc_reg`/`dec_reg` exist and pass tests. ✅ (Task 1)
2. `ShiftOp` and `shift_reg_imm8`/`shift_reg_cl` exist and pass tests for all 3 operations (imm8) plus at least one CL-form test. ✅ (Task 2)
3. `lea_reg_mem` exists and is tested to confirm it's genuinely `lea`, not `mov`. ✅ (Task 3)
4. `lea_reg_scaled` exists; a real scaled-index case, the RSP-as-index assert, and the combined rbp/r13-disp0-with-real-index case are all tested. ✅ (Task 4)
5. `imul128_reg`/`idiv_reg`/`cqo` exist and pass tests. ✅ (Task 5)
6. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 1-3)
7. No regressions in 6a's/6b's/6c's existing tests or any other crate's tests. ✅ (Step 4)
