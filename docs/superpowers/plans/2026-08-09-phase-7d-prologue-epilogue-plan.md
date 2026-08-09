# forge Phase 7d Prologue/Epilogue & ABI Frame Plumbing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `emit_prologue`/`emit_epilogue` in a new `crates/forge-x64/src/prologue.rs`, calling existing `Assembler` methods to produce real, ABI-correct System V function prologue/epilogue bytes, parameterized by `(callee_saved: &[PhysReg], spill_bytes: u32)`.

**Architecture:** This is encoder-layer work (unlike Phase 7a-7c's `MachineInst`-selection layer) — `emit_prologue`/`emit_epilogue` call `push_reg`/`pop_reg`/`mov_reg_reg`/`alu_reg_imm`/`ret` directly. A shared pure function `padded_spill_bytes` computes the 16-byte-aligned frame size, called identically by both, so they can never disagree about the total frame size. The epilogue explicitly pops each callee-saved register in reverse order (never a `mov rsp, rbp` shortcut, which would silently skip restoring them once any exist).

**Tech Stack:** Rust, `iced-x86` (dev-dependency, disassembler oracle only).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-7d-prologue-epilogue-design.md` — read this first. This design was reviewed and had its core byte-level algorithm independently traced across 4 concrete `(callee_saved, spill_bytes)` configurations, with no correctness bugs found — trust the algorithm; this plan's golden bytes below were independently hand-derived from the actual `push_reg`/`pop_reg`/`mov_reg_reg`/`alu_reg_imm` implementations in `crates/forge-x64/src/assembler.rs` (not guessed), and several match well-known standard x86-64 encodings (`mov rbp,rsp` = `48 89 E5`, `push rbp` = `55`, etc.) as an independent sanity check.

---

## Task 1: `padded_spill_bytes`, `emit_prologue`, `emit_epilogue`, `SYSV_CALLEE_SAVED`

**Files:**
- Create: `crates/forge-x64/src/prologue.rs`
- Modify: `crates/forge-x64/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/prologue.rs — append at the end of the file, after the code from Step 3

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AluOp, Assembler};

    #[test]
    fn sysv_callee_saved_excludes_rbp() {
        assert_eq!(
            SYSV_CALLEE_SAVED,
            &[PhysReg::Rbx, PhysReg::R12, PhysReg::R13, PhysReg::R14, PhysReg::R15]
        );
        assert!(!SYSV_CALLEE_SAVED.contains(&PhysReg::Rbp));
    }

    #[test]
    fn padded_spill_bytes_needs_no_padding_when_already_aligned() {
        assert_eq!(padded_spill_bytes(0, 0), 0);
        assert_eq!(padded_spill_bytes(0, 32), 32);
        assert_eq!(padded_spill_bytes(2, 0), 0); // even callee-saved count, already aligned
    }

    #[test]
    fn padded_spill_bytes_pads_odd_callee_saved_count() {
        // 1 callee-saved reg (8 bytes) is itself misaligned by 8; even
        // requesting 0 spill bytes needs 8 bytes of padding to re-align.
        assert_eq!(padded_spill_bytes(1, 0), 8);
    }

    #[test]
    fn padded_spill_bytes_pads_misaligned_request_up_to_the_next_16() {
        // 20 is not a multiple of 16; padded up to 32.
        assert_eq!(padded_spill_bytes(0, 20), 32);
    }

    #[test]
    fn emit_prologue_and_epilogue_degenerate_case_no_callee_saved_no_spill() {
        let mut prologue = Assembler::new();
        emit_prologue(&mut prologue, &[], 0);
        assert_eq!(
            prologue.code(),
            &[
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
            ]
        );

        let mut epilogue = Assembler::new();
        emit_epilogue(&mut epilogue, &[], 0);
        assert_eq!(
            epilogue.code(),
            &[
                0x5D, // pop rbp
                0xC3, // ret
            ]
        );
    }

    #[test]
    fn emit_prologue_and_epilogue_already_aligned_spill_no_callee_saved() {
        let mut prologue = Assembler::new();
        emit_prologue(&mut prologue, &[], 32);
        assert_eq!(
            prologue.code(),
            &[
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x48, 0x83, 0xEC, 0x20, // sub rsp, 32
            ]
        );

        let mut epilogue = Assembler::new();
        emit_epilogue(&mut epilogue, &[], 32);
        assert_eq!(
            epilogue.code(),
            &[
                0x48, 0x83, 0xC4, 0x20, // add rsp, 32
                0x5D, // pop rbp
                0xC3, // ret
            ]
        );
    }

    /// Requesting 20 (not a multiple of 16) must produce byte-IDENTICAL
    /// output to requesting 32 directly (the padded-up value) -- this is
    /// the clearest possible proof the padding math actually ran, not
    /// just that some sub/add was emitted.
    #[test]
    fn emit_prologue_pads_a_misaligned_spill_request_up_to_32() {
        let mut requested_20 = Assembler::new();
        emit_prologue(&mut requested_20, &[], 20);

        let mut requested_32 = Assembler::new();
        emit_prologue(&mut requested_32, &[], 32);

        assert_eq!(requested_20.code(), requested_32.code());
    }

    #[test]
    fn emit_prologue_and_epilogue_odd_callee_saved_count_pads_for_alignment() {
        let mut prologue = Assembler::new();
        emit_prologue(&mut prologue, &[PhysReg::Rbx], 0);
        assert_eq!(
            prologue.code(),
            &[
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x53, // push rbx
                0x48, 0x83, 0xEC, 0x08, // sub rsp, 8 (padding for the odd count)
            ]
        );

        let mut epilogue = Assembler::new();
        emit_epilogue(&mut epilogue, &[PhysReg::Rbx], 0);
        assert_eq!(
            epilogue.code(),
            &[
                0x48, 0x83, 0xC4, 0x08, // add rsp, 8
                0x5B, // pop rbx
                0x5D, // pop rbp
                0xC3, // ret
            ]
        );
    }

    #[test]
    fn emit_prologue_and_epilogue_even_callee_saved_count_needs_no_padding() {
        let mut prologue = Assembler::new();
        emit_prologue(&mut prologue, &[PhysReg::Rbx, PhysReg::R12], 0);
        assert_eq!(
            prologue.code(),
            &[
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x53, // push rbx
                0x41, 0x54, // push r12
            ]
        );

        let mut epilogue = Assembler::new();
        emit_epilogue(&mut epilogue, &[PhysReg::Rbx, PhysReg::R12], 0);
        assert_eq!(
            epilogue.code(),
            &[
                0x41, 0x5C, // pop r12 (reverse order: r12 first, since it was pushed last)
                0x5B, // pop rbx
                0x5D, // pop rbp
                0xC3, // ret
            ]
        );
    }

    #[test]
    #[should_panic(expected = "Rbp must not appear")]
    fn emit_prologue_panics_if_rbp_is_in_callee_saved() {
        let mut asm = Assembler::new();
        emit_prologue(&mut asm, &[PhysReg::Rbp], 0);
    }

    #[test]
    #[should_panic(expected = "Rbp must not appear")]
    fn emit_epilogue_panics_if_rbp_is_in_callee_saved() {
        let mut asm = Assembler::new();
        emit_epilogue(&mut asm, &[PhysReg::Rbp], 0);
    }

    /// Local copy of the round_trip.rs disassembly helper -- that file is
    /// a SEPARATE integration test binary (crates/forge-x64/tests/), not
    /// reachable from this crate's own unit tests, so it's duplicated
    /// here rather than referenced across the boundary.
    fn disassemble(bytes: &[u8]) -> Vec<String> {
        use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, NasmFormatter};
        let mut decoder = Decoder::with_ip(64, bytes, 0, DecoderOptions::NONE);
        let mut formatter = NasmFormatter::new();
        let mut result = Vec::new();
        let mut instruction = Instruction::default();
        while decoder.can_decode() {
            decoder.decode_out(&mut instruction);
            let mut text = String::new();
            formatter.format(&instruction, &mut text);
            result.push(text);
        }
        result
    }

    /// Full round trip through iced-x86: confirms both the byte sequence
    /// AND the disassembled mnemonics/operand order read as a sane,
    /// symmetric prologue/epilogue pair for a realistic multi-register
    /// case -- not just that the raw bytes match a hand-derived array.
    #[test]
    fn full_round_trip_disassembles_as_a_symmetric_prologue_epilogue_pair() {
        let mut asm = Assembler::new();
        emit_prologue(&mut asm, &[PhysReg::Rbx, PhysReg::R12], 0);
        emit_epilogue(&mut asm, &[PhysReg::Rbx, PhysReg::R12], 0);

        // NOTE: verify this disassembly empirically -- iced-x86's exact
        // mnemonic/operand-order rendering for this specific instruction
        // sequence wasn't checked against a live compile when this plan
        // was written, though every individual instruction shape here
        // (push/pop/mov reg,reg) has already been exercised and verified
        // correct in Phase 6a-6f's own round_trip.rs tests.
        assert_eq!(
            disassemble(asm.code()),
            vec![
                "push rbp",
                "mov rbp,rsp",
                "push rbx",
                "push r12",
                "pop r12",
                "pop rbx",
                "pop rbp",
                "ret",
            ]
        );
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — `prologue` module doesn't exist yet (compile error).

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/prologue.rs — full file contents (tests from Step 1 go at the end)

use crate::{AluOp, Assembler, PhysReg};

/// Callee-saved GPRs per System V AMD64 -- does NOT include Rbp, whose
/// save/restore is handled unconditionally by emit_prologue/emit_epilogue
/// themselves, never by the caller passing it in this list. Win64 support
/// (a different, larger callee-saved set including XMM6-15, which need a
/// movsd-to-memory save sequence rather than push/pop) is deliberately
/// not built here -- see the design doc for why.
pub const SYSV_CALLEE_SAVED: &[PhysReg] =
    &[PhysReg::Rbx, PhysReg::R12, PhysReg::R13, PhysReg::R14, PhysReg::R15];

/// Computes the actual byte count `sub rsp, N` / `add rsp, N` should use:
/// `requested`, padded up to the next value making the TOTAL frame
/// (callee-saved pushes + this) a multiple of 16 -- see the design doc's
/// "Stack alignment" section for the derivation. A pure function, not a
/// method, so emit_prologue and emit_epilogue each independently compute
/// the identical value from the identical inputs and can never disagree.
///
/// PRECONDITION: `requested` must be a realistic spill-slot frame size
/// (nowhere near `u32::MAX`) -- plain `u32` addition here is unchecked,
/// so a `requested` within 16 bytes of `u32::MAX` could overflow/wrap.
/// Not a concern for any real JIT'd expression's spill footprint (which
/// would stack-overflow long before approaching 4GB), so this is
/// documented as a precondition rather than defended with checked
/// arithmetic against an unreachable input.
fn padded_spill_bytes(num_callee_saved: usize, requested: u32) -> u32 {
    let base_offset = (num_callee_saved as u32) * 8;
    let misalignment = (base_offset + requested) % 16;
    if misalignment == 0 {
        requested
    } else {
        requested + (16 - misalignment)
    }
}

/// Emits `push rbp; mov rbp, rsp; <push each callee_saved reg>; [sub rsp, N]`.
/// `callee_saved` must not contain Rbp (see module doc). `spill_bytes` is
/// the RAW requested spill-slot size -- this function pads it internally
/// for 16-byte alignment; callers should NOT pre-pad it themselves.
pub fn emit_prologue(asm: &mut Assembler, callee_saved: &[PhysReg], spill_bytes: u32) {
    assert!(
        !callee_saved.contains(&PhysReg::Rbp),
        "Rbp must not appear in callee_saved -- its save/restore is \
         handled unconditionally by emit_prologue/emit_epilogue themselves"
    );
    asm.push_reg(PhysReg::Rbp);
    asm.mov_reg_reg(PhysReg::Rbp, PhysReg::Rsp);
    for &reg in callee_saved {
        asm.push_reg(reg);
    }
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    if n > 0 {
        asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, n as i32);
    }
}

/// Emits `[add rsp, N]; <pop each callee_saved reg, REVERSE order>; pop rbp; ret`.
/// Must be called with the EXACT SAME `callee_saved`/`spill_bytes` as the
/// matching emit_prologue call -- both independently compute the same
/// padded N via padded_spill_bytes, so as long as the inputs match, the
/// two are guaranteed symmetric.
pub fn emit_epilogue(asm: &mut Assembler, callee_saved: &[PhysReg], spill_bytes: u32) {
    assert!(
        !callee_saved.contains(&PhysReg::Rbp),
        "Rbp must not appear in callee_saved -- its save/restore is \
         handled unconditionally by emit_prologue/emit_epilogue themselves"
    );
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    if n > 0 {
        asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, n as i32);
    }
    for &reg in callee_saved.iter().rev() {
        asm.pop_reg(reg);
    }
    asm.pop_reg(PhysReg::Rbp);
    asm.ret();
}
```

- [ ] **Step 4: Wire the module into `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod machine_inst;
mod prologue;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, RoundMode, ShiftOp, SseOp};
pub use machine_inst::{select, ConstantPool, MachineInst, PoolIndex, SelectedFunction};
pub use prologue::{emit_epilogue, emit_prologue, SYSV_CALLEE_SAVED};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -40`
Expected: all 11 new tests pass (`sysv_callee_saved_excludes_rbp`, `padded_spill_bytes_needs_no_padding_when_already_aligned`, `padded_spill_bytes_pads_odd_callee_saved_count`, `padded_spill_bytes_pads_misaligned_request_up_to_the_next_16`, `emit_prologue_and_epilogue_degenerate_case_no_callee_saved_no_spill`, `emit_prologue_and_epilogue_already_aligned_spill_no_callee_saved`, `emit_prologue_pads_a_misaligned_spill_request_up_to_32`, `emit_prologue_and_epilogue_odd_callee_saved_count_pads_for_alignment`, `emit_prologue_and_epilogue_even_callee_saved_count_needs_no_padding`, `emit_prologue_panics_if_rbp_is_in_callee_saved`, `emit_epilogue_panics_if_rbp_is_in_callee_saved`, `full_round_trip_disassembles_as_a_symmetric_prologue_epilogue_pair` — that's actually 12, recount against the real `cargo test` output rather than this list if they diverge).

- [ ] **Step 6: Run the FULL workspace test suite to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 7: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 8: Commit**

```bash
git add crates/forge-x64/src/prologue.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): emit_prologue, emit_epilogue, SYSV_CALLEE_SAVED"
```

## Context for this task

This is the only task in this plan — Phase 7d is small enough not to need multiple tasks. Every golden byte in the tests above was hand-derived directly from `crates/forge-x64/src/assembler.rs`'s real `push_reg`/`pop_reg`/`mov_reg_reg`/`alu_reg_imm` implementations (not guessed), and several match textbook/compiler-standard x86-64 encodings as a sanity check (`mov rbp,rsp` = `48 89 E5`, `push rbp` = `55`, `sub rsp,N` via the imm8 form = `48 83 EC <N>`) — if any assertion fails, suspect a transcription slip in this plan before suspecting the underlying encoder (which is Phase 6 work, already exhaustively tested).

The disassembly-string assertion in the last test is the one place needing empirical verification per this project's established discipline — everything else (byte arrays) is pure arithmetic, already correct by construction.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -60`. Report exact final counts.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm all 8 exit criteria from the design doc are met:
1. `emit_prologue`/`emit_epilogue` exist in `crates/forge-x64/src/prologue.rs`, exported from `lib.rs`.
2. Both correctly save/restore an arbitrary `callee_saved: &[PhysReg]` list (excluding `Rbp`) via real `push_reg`/`pop_reg` calls, in matching forward/reverse order.
3. `padded_spill_bytes` correctly pads for 16-byte total-frame alignment; both emit functions use it identically.
4. Both functions `assert!` if `Rbp` appears in `callee_saved`.
5. `SYSV_CALLEE_SAVED` constant exists with the correct 5-register set.
6. Tests cover the degenerate case, already-aligned spill sizes, misaligned spill sizes needing padding, odd/even callee-saved counts, a full round trip, and both panic cases.
7. `cargo test --workspace` green, clippy/fmt clean.
8. No regressions in any Phase 6/7a/7b/7c `forge-x64` test or any other crate's tests.
