# forge Phase 6f x86-64 Calling Convention & RIP-Relative Addressing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the last two still-unbuilt 🔴-blocking items in CHECKLIST.md's Phase 6 ("x86-64 Encoder") — `push`/`pop`/`call`/`ret` and RIP-relative addressing — closing out Phase 6's blocking work before Phase 7 ("Instruction Selection & Prologue") starts.

**Architecture:** All new methods live in `crates/forge-x64/src/assembler.rs`, in new `impl Assembler` blocks appended at the end of the file. `push_reg`/`pop_reg`/`call_reg`/`call_rel32`/`ret` reuse `rex()`/`modrm_reg()`/`Label`/`Fixup`/`bind()` exactly as before, with no changes to any of them. `lea_reg_riprel`/`movsd_reg_riprel` introduce RIP-relative addressing (ModRM `mod=00, rm=101`, a fixed bit pattern rather than a real register's encoding) but reuse the *exact same* `Label`/`Fixup`/`bind()` fixup-patching machinery `jmp` already established in Phase 6a — no new fixup kind, no changes to `bind()`/`patch_fixup()`.

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only — already wired in Phase 6a).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-6f-x64-calling-convention-riprel-design.md` — read this first.

**A note on running test counts:** every task below states an "Expected" pass count computed from the prior task's estimate, starting from the confirmed baseline of 14 lib + 96 integration tests at the end of Phase 6e (110 total `forge-x64` tests). In every prior Phase 6 sub-slice, review rounds sometimes added extra tests beyond a task's original estimate, making later tasks' arithmetic stale. Treat every count in this plan as a best-effort estimate, not ground truth: always trust the actual output of `cargo test -p forge-x64`, and if a later task's baseline looks wrong, check `git log` for the actual test count in the prior task's final commit rather than assuming this plan's running arithmetic is right.

---

## Task 1: `push_reg` / `pop_reg`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn push_reg_low_register_needs_no_rex() {
    let mut a = Assembler::new();
    a.push_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0x50]);
    assert_eq!(disassemble(a.code()), vec!["push rax"]);
}

#[test]
fn push_reg_extended_register_sets_rex_b() {
    let mut a = Assembler::new();
    a.push_reg(PhysReg::R12);
    assert_eq!(a.code(), &[0x41, 0x54]);
    assert_eq!(disassemble(a.code()), vec!["push r12"]);
}

#[test]
fn pop_reg_low_register_needs_no_rex() {
    let mut a = Assembler::new();
    a.pop_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0x58]);
    assert_eq!(disassemble(a.code()), vec!["pop rax"]);
}

#[test]
fn pop_reg_extended_register_sets_rex_b() {
    let mut a = Assembler::new();
    a.pop_reg(PhysReg::R12);
    assert_eq!(a.code(), &[0x41, 0x5C]);
    assert_eq!(disassemble(a.code()), vec!["pop r12"]);
}
```

**IMPORTANT — before trusting the disassembly strings above**: verify all four empirically. This shape (opcode-plus-register, no ModRM, possible REX byte) already exists in this crate — `mov_reg_imm`'s `movabs` form uses it too, and is already tested with an extended register — so this is low risk, but confirm nothing unexpected happens with the `iced-x86` formatter here before trusting it.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `push_reg`/`pop_reg` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add a new `impl Assembler` block at the end of the file, after the block containing `roundsd`

impl Assembler {
    /// `push src` -- 50+r, no ModRM (register encoded in the opcode's
    /// low 3 bits, the same shape mov_reg_imm's opcode byte uses). No
    /// REX.W -- push/pop default to 64-bit operand size in long mode;
    /// REX.W has no defined effect on this opcode. REX.B only if src
    /// is r8-r15.
    pub fn push_reg(&mut self, src: PhysReg) {
        self.rex(false, 0, 0, src.encoding());
        self.code.push(0x50 + (src.encoding() & 7));
    }

    /// `pop dst` -- 58+r, the mirror image of push_reg.
    pub fn pop_reg(&mut self, dst: PhysReg) {
        self.rex(false, 0, 0, dst.encoding());
        self.code.push(0x58 + (dst.encoding() & 7));
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib tests unchanged + 100 integration tests: 96 existing + 4 new).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): push_reg, pop_reg"
```

## Context for this task

This is the foundational task for this slice — the first `impl Assembler` block for calling-convention instructions, which Task 2 will extend with `call_reg`/`call_rel32`/`ret`. `push_reg`/`pop_reg` are opcode-plus-register forms with NO ModRM byte at all — the same shape `mov_reg_imm`'s `movabs` form already uses (see `assembler.rs`'s existing `mov_reg_imm`), so the REX-without-ModRM mechanics are already proven in this crate; this task just applies that same shape to a new opcode pair. If `push_reg_extended_register_sets_rex_b` fails, check that the REX byte and the opcode-plus-register byte are in the right order (REX always immediately precedes the opcode, same rule established in 6e) and that `& 7` is masking the register encoding correctly for the opcode's low 3 bits.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `call_reg` / `call_rel32` / `ret`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn call_reg_low_register_needs_no_rex() {
    let mut a = Assembler::new();
    a.call_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0xFF, 0xD0]);
    assert_eq!(disassemble(a.code()), vec!["call rax"]);
}

#[test]
fn call_reg_extended_register_sets_rex_b() {
    let mut a = Assembler::new();
    a.call_reg(PhysReg::R12);
    assert_eq!(a.code(), &[0x41, 0xFF, 0xD4]);
    assert_eq!(disassemble(a.code()), vec!["call r12"]);
}

/// Forward reference: the label isn't bound yet when call_rel32 runs,
/// so it must record a Fixup (exactly like jmp's forward-jump branch)
/// and patch it once bind() runs. Assertions are formulaic (derived
/// from the actual buffer layout), not hand-typed magic numbers, per
/// this crate's established jmp/jcc test style -- plus one concrete
/// sanity value so the derivation itself can be double-checked by hand.
#[test]
fn call_rel32_forward_reference_patches_correctly() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.call_rel32(label);
    a.ret();
    a.bind(label);
    assert_eq!(a.code()[0], 0xE8);
    let fixup_at = 1; // opcode byte is code[0]; the 4-byte rel32 starts at code[1]
    let end_of_fixup = fixup_at + 4; // == 5, where `ret`'s single byte begins
    let target_pos = a.code().len(); // bind() ran right after ret(), so this is the label's position
    let expected_rel32 = (target_pos as isize - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    // Sanity check on the derivation: call_rel32 emits 5 bytes, ret emits 1,
    // so target_pos == 6 and end_of_fixup == 5, giving rel32 == 1.
    assert_eq!(expected_rel32, 1);
    // NOTE: verify this string empirically -- unlike every other disassembly
    // string in this crate so far, this one depends on the disassembler's
    // assumed instruction pointer for resolving the call's absolute target,
    // which wasn't checked against a live compile when this plan was written.
    assert_eq!(disassemble(a.code()), vec!["call 6", "ret"]);
}

/// Backward reference: the label is already bound when call_rel32 runs,
/// so the distance is computed immediately with no Fixup involved at
/// all -- mirrors jmp's backward-jump branch.
#[test]
fn call_rel32_backward_reference_computes_immediately() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.bind(label);
    a.ret();
    a.call_rel32(label);
    let fixup_at = a.code().len() - 4;
    let end_of_fixup = fixup_at + 4;
    let target_pos = 0isize; // label was bound at the very start of the buffer
    let expected_rel32 = (target_pos - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    // Sanity check: ret (1 byte) + call_rel32 (5 bytes) == 6 bytes total,
    // so end_of_fixup == 6 and target_pos == 0, giving rel32 == -6.
    assert_eq!(expected_rel32, -6);
}

#[test]
fn ret_encodes_correctly() {
    let mut a = Assembler::new();
    a.ret();
    assert_eq!(a.code(), &[0xC3]);
    assert_eq!(disassemble(a.code()), vec!["ret"]);
}
```

**IMPORTANT**: verify all disassembly strings empirically before committing. `call_rel32_forward_reference_patches_correctly`'s string is flagged as the least certain one in this task — if the exact rendering differs (e.g. a different number base, or an explicit `short`/`near` qualifier), correct the string to match reality; the byte-level assertions above it are what actually prove correctness and don't depend on the guess.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `call_reg`/`call_rel32`/`ret` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `push_reg`

impl Assembler {
    /// `call target` -- FF /2, indirect through a register holding an
    /// absolute address. This is how forge calls libm: mov_reg_imm the
    /// function pointer into a register, then call_reg through it --
    /// a direct rel32 call can't reliably reach an arbitrary libm
    /// address (it may be outside +/-2GiB of the JIT buffer).
    pub fn call_reg(&mut self, target: PhysReg) {
        self.rex(false, 0, 0, target.encoding());
        self.code.push(0xFF);
        self.modrm_reg(2, target.encoding());
    }

    /// `call label` -- E8 rel32, direct call within the same code
    /// buffer (e.g. a future JIT-to-JIT call). Unlike jmp/jcc there is
    /// no rel8 short form for call at all -- this is unconditionally
    /// the 5-byte form, whether the label is already bound (backward,
    /// distance computed immediately) or not (forward, recorded as a
    /// Fixup exactly like jmp's forward-jump branch).
    pub fn call_rel32(&mut self, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end = self.code.len() + 5;
            let rel32 = target_pos as isize - end as isize;
            self.code.push(0xE8);
            self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
        } else {
            self.code.push(0xE8);
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]);
            self.fixups.push(Fixup { at, target: label });
        }
    }

    /// `ret` -- C3, no operands. The rare imm16 stack-cleanup form
    /// (cdecl-style callee cleanup) is not used by SysV or Win64 and
    /// isn't built.
    pub fn ret(&mut self) {
        self.code.push(0xC3);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 100 existing integration + 5 new = 105 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): call_reg, call_rel32, ret"
```

## Context for this task

`call_rel32` reuses the `Fixup` struct and `self.fixups`/`self.labels` fields directly (both are private fields on `Assembler`, accessible from within `assembler.rs` since this method lives in the same file/module) — do not add a new field or struct for this. It's structured identically to `jmp`'s two branches, just simpler: no rel8/rel32 auto-selection, since `call` only has the rel32 form. If you're unsure how `jmp` and `bind()` interact, read them first (both are in this same file, in the `impl Assembler` block containing `bind`) — `call_rel32` must produce fixups that `bind()` (unmodified) can already patch correctly, since `bind()` doesn't know or care which instruction created a given `Fixup`.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `lea_reg_riprel` / `movsd_reg_riprel`

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn lea_reg_riprel_forward_reference_patches_correctly() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.lea_reg_riprel(PhysReg::Rax, label);
    a.ret();
    a.bind(label);
    assert_eq!(&a.code()[0..3], &[0x48, 0x8D, 0x05]);
    let fixup_at = 3;
    let end_of_fixup = fixup_at + 4; // == 7, where `ret`'s single byte begins
    let target_pos = a.code().len(); // bind() ran right after ret()
    let expected_rel32 = (target_pos as isize - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    // Sanity check: lea_reg_riprel emits 7 bytes, ret emits 1, so
    // target_pos == 8 and end_of_fixup == 7, giving rel32 == 1.
    assert_eq!(expected_rel32, 1);
    // NOTE: verify this string empirically -- this is the single
    // riskiest disassembly guess in the whole Phase 6 encoder so far.
    // iced-x86's NasmFormatter may render a RIP-relative operand as a
    // relative offset ("[rip+1]"), as a resolved absolute target
    // address, or some other form entirely -- none of this was checked
    // against a live compile when this plan was written. If this
    // string is wrong, fix it to match reality; the byte-level and
    // formulaic-offset assertions above are what actually prove the
    // encoding is correct.
    assert_eq!(disassemble(a.code()), vec!["lea rax,[rip+1]", "ret"]);
}

/// Proves REX.R (not REX.B) is what threads through for an extended
/// destination register -- the exact risk the design doc calls out:
/// mod=00/rm=101's rm field is a fixed CPU bit pattern, not a real
/// register encoding, so REX.B must never be set by this method
/// regardless of which register `dst` is.
#[test]
fn lea_reg_riprel_extended_register_sets_only_rex_r_not_rex_b() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.lea_reg_riprel(PhysReg::R9, label);
    a.ret();
    a.bind(label);
    // 0x4C == 0x40 | REX.W(0x08) | REX.R(0x04) -- REX.B (bit 0) is NOT set.
    assert_eq!(&a.code()[0..3], &[0x4C, 0x8D, 0x0D]);
    let fixup_at = 3;
    let end_of_fixup = fixup_at + 4;
    let target_pos = a.code().len();
    let expected_rel32 = (target_pos as isize - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    assert_eq!(expected_rel32, 1);
    // NOTE: verify this string empirically, same caveat as the test above.
    assert_eq!(disassemble(a.code()), vec!["lea r9,[rip+1]", "ret"]);
}

#[test]
fn movsd_reg_riprel_forward_reference_patches_correctly() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.movsd_reg_riprel(PhysReg::Xmm0, label);
    a.ret();
    a.bind(label);
    assert_eq!(&a.code()[0..4], &[0xF2, 0x0F, 0x10, 0x05]);
    let fixup_at = 4;
    let end_of_fixup = fixup_at + 4; // == 8, where `ret`'s single byte begins
    let target_pos = a.code().len();
    let expected_rel32 = (target_pos as isize - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    // Sanity check: movsd_reg_riprel emits 8 bytes, ret emits 1, so
    // target_pos == 9 and end_of_fixup == 8, giving rel32 == 1.
    assert_eq!(expected_rel32, 1);
    // NOTE: verify this string empirically, same caveat as lea_reg_riprel's.
    assert_eq!(disassemble(a.code()), vec!["movsd xmm0,[rip+1]", "ret"]);
}

/// Backward reference: the label is already bound when
/// movsd_reg_riprel runs, so riprel_fixup's immediate branch computes
/// the distance directly with no Fixup involved -- mirrors
/// call_rel32_backward_reference_computes_immediately.
#[test]
fn movsd_reg_riprel_backward_reference_computes_immediately() {
    let mut a = Assembler::new();
    let label = a.new_label();
    a.bind(label);
    a.ret();
    a.movsd_reg_riprel(PhysReg::Xmm0, label);
    let fixup_at = a.code().len() - 4;
    let end_of_fixup = fixup_at + 4;
    let target_pos = 0isize; // label was bound at the very start of the buffer
    let expected_rel32 = (target_pos - end_of_fixup as isize) as i32;
    let actual_rel32 = i32::from_le_bytes(a.code()[fixup_at..end_of_fixup].try_into().unwrap());
    assert_eq!(actual_rel32, expected_rel32);
    // Sanity check: ret (1 byte) + movsd_reg_riprel (8 bytes) == 9 bytes
    // total, so end_of_fixup == 9 and target_pos == 0, giving rel32 == -9.
    assert_eq!(expected_rel32, -9);
}
```

**CRITICAL**: the `lea`/`movsd` disassembly strings above are genuinely unverified guesses — this is the riskiest disassembly-format uncertainty in the whole Phase 6 encoder so far (more uncertain than Phase 6e's `roundsd` immediate-formatting question, since here the whole *addressing mode* representation is unverified, not just a number's base). Verify all three empirically before committing. If `iced-x86` renders these differently (e.g. showing a resolved absolute address instead of a `rip+`-relative offset), correct the strings — the byte-level and formulaic-offset assertions in each test are what actually prove the encoding is correct, and are not dependent on the disassembly string being right.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `lea_reg_riprel`/`movsd_reg_riprel` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add a new `impl Assembler` block at the end of the file, after the block containing `ret`

impl Assembler {
    /// `lea dst, [rip + disp32]` where disp32 resolves label -- REX.W +
    /// 8D /r with a fixed mod=00/rm=101 ModRM byte. REX.B is
    /// deliberately never set here (rex() is called with rm=0) since
    /// mod=00/rm=101 is not a real register's encoding -- it's the
    /// CPU's dedicated RIP-relative bit pattern in 64-bit mode, and
    /// setting REX.B would be meaningless/undefined for it. Reuses
    /// lea_reg_mem's REX.W=true and opcode 0x8D.
    pub fn lea_reg_riprel(&mut self, dst: PhysReg, label: Label) {
        self.rex(true, dst.encoding(), 0, 0);
        self.code.push(0x8D);
        self.code.push(0b00_000_101 | ((dst.encoding() & 7) << 3));
        self.riprel_fixup(label);
    }

    /// `movsd dst, [rip + disp32]` -- F2 0F 10 /r with the same
    /// mod=00/rm=101 RIP-relative ModRM shape as lea_reg_riprel.
    /// Reuses movsd_reg_reg's F2 prefix, REX.W=false, and opcode 0x10.
    pub fn movsd_reg_riprel(&mut self, dst: PhysReg, label: Label) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, 0);
        self.code.push(0x0F);
        self.code.push(0x10);
        self.code.push(0b00_000_101 | ((dst.encoding() & 7) << 3));
        self.riprel_fixup(label);
    }

    /// Shared by both RIP-relative methods above: emits the trailing
    /// disp32 (immediately, if `label` is already bound -- an unusual
    /// case for a constant pool placed after the code, but handled for
    /// correctness/symmetry with jmp/call_rel32) or a 4-byte
    /// placeholder plus a Fixup (the expected case, since Phase 7's
    /// constant pool is placed after the code it's referenced from).
    /// Correct ONLY when disp32 is the last bytes of the instruction --
    /// true for both current callers, since neither has a trailing
    /// immediate after its memory operand. A future RIP-relative
    /// consumer WITH a trailing immediate (e.g. an imm32 ALU op reading
    /// a RIP-relative operand) would need a different fixup scheme;
    /// document this constraint rather than generalizing prematurely.
    fn riprel_fixup(&mut self, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end = self.code.len() + 4;
            let rel32 = target_pos as isize - end as isize;
            self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
        } else {
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]);
            self.fixups.push(Fixup { at, target: label });
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (14 lib + 105 existing integration + 4 new = 109 integration).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): lea_reg_riprel, movsd_reg_riprel"
```

## Context for this task

This is the highest-risk task in the plan, for two independent reasons: (1) it's the first genuinely new *addressing mode* in this crate (not just a new instruction reusing an existing one), so a mistake in the fixed `mod=00/rm=101` ModRM pattern would be easy to get subtly wrong; (2) the disassembly-string guesses are the least certain in the whole Phase 6 encoder so far — do not skip empirical verification even if the golden bytes pass on the first try.

`riprel_fixup` is a private helper shared by both public methods — do not duplicate its logic inline in each one. It deliberately reuses the exact same `self.fixups`/`self.labels` fields and `Fixup` struct that `jmp`/`call_rel32` already use, with no new types. If `lea_reg_riprel_extended_register_sets_only_rex_r_not_rex_b` fails, the most likely bug is passing `dst.encoding()` into `rex()`'s `rm` parameter instead of `0` — that would incorrectly set REX.B for extended registers, which is exactly the bug this test exists to catch.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including all of `forge-x64`'s new tests. Report the exact final counts — per this plan's header note, trust the actual run over the plan's per-task arithmetic.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Confirm no regressions in 6a's/6b's/6c's/6d's/6e's work**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | tail -10` (confirm every named test family from 6a through 6e is still present and passing alongside 6f's new tests) and `make spike` (confirm the Phase 0 day-one spike still works).

- [ ] **Step 5: Report exit criteria status**

Confirm all 7 exit criteria from the design doc are met:
1. `push_reg`/`pop_reg` exist and pass tests, including an extended-register case for each.
2. `call_reg` (indirect) and `call_rel32` (direct, via `Label`) both exist and pass tests, including a forward and backward reference for `call_rel32`.
3. `ret` exists and passes a test.
4. `lea_reg_riprel`/`movsd_reg_riprel` exist, pass tests, and correctly reuse the `Label`/`Fixup`/`bind()` machinery from 6a with no changes to `bind()`, `Fixup`, or `patch_fixup()` themselves.
5. `cargo test --workspace` green, clippy/fmt clean.
6. No regressions in 6a-6e's existing tests or any other crate's tests.
7. CHECKLIST.md's `push`/`pop`/`call`/`ret` bullet and the "RIP-relative addressing for constant pool loads" bullet are annotated to reflect what was actually built in this slice, matching the note/correction pattern used at the end of every prior Phase 6 sub-slice.
