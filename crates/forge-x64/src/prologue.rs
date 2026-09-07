use crate::{AluOp, Assembler, PhysReg};

/// Callee-saved GPRs per System V AMD64 -- does NOT include Rbp, whose
/// save/restore is handled unconditionally by emit_prologue/emit_epilogue
/// themselves, never by the caller passing it in this list. On Windows the
/// active callee-saved list also contains XMM6-XMM15; the Windows prologue
/// stores those registers in the frame because XMM registers cannot be pushed.
pub const SYSV_CALLEE_SAVED: &[PhysReg] = &[
    PhysReg::Rbx,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

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

    #[cfg(windows)]
    {
        let gpr_count = callee_saved.iter().filter(|reg| !is_xmm(**reg)).count();
        let xmm_saved: Vec<PhysReg> = callee_saved
            .iter()
            .copied()
            .filter(|reg| is_xmm(*reg))
            .collect();
        for &reg in callee_saved.iter().filter(|reg| !is_xmm(**reg)) {
            asm.push_reg(reg);
        }
        let xmm_bytes = (xmm_saved.len() as u32) * 8;
        let n = padded_spill_bytes(gpr_count, spill_bytes + xmm_bytes);
        if n > 0 {
            asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, n as i32);
        }
        for (index, reg) in xmm_saved.iter().enumerate() {
            let offset = -i32::try_from((gpr_count + index + 1) * 8)
                .expect("Win64 nonvolatile XMM frame is too large");
            asm.movsd_mem_reg(PhysReg::Rbp, offset, *reg);
        }
        return;
    }

    #[cfg(not(windows))]
    for &reg in callee_saved {
        asm.push_reg(reg);
    }
    #[cfg(not(windows))]
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    #[cfg(not(windows))]
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
    #[cfg(windows)]
    {
        let gpr_count = callee_saved.iter().filter(|reg| !is_xmm(**reg)).count();
        let xmm_saved: Vec<PhysReg> = callee_saved
            .iter()
            .copied()
            .filter(|reg| is_xmm(*reg))
            .collect();
        let xmm_bytes = (xmm_saved.len() as u32) * 8;
        for (index, reg) in xmm_saved.iter().enumerate().rev() {
            let offset = -i32::try_from((gpr_count + index + 1) * 8)
                .expect("Win64 nonvolatile XMM frame is too large");
            asm.movsd_reg_mem(*reg, PhysReg::Rbp, offset);
        }
        let n = padded_spill_bytes(gpr_count, spill_bytes + xmm_bytes);
        if n > 0 {
            asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, n as i32);
        }
        for &reg in callee_saved.iter().filter(|reg| !is_xmm(**reg)).rev() {
            asm.pop_reg(reg);
        }
        asm.pop_reg(PhysReg::Rbp);
        asm.ret();
        return;
    }

    #[cfg(not(windows))]
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    #[cfg(not(windows))]
    if n > 0 {
        asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, n as i32);
    }
    #[cfg(not(windows))]
    for &reg in callee_saved.iter().rev() {
        asm.pop_reg(reg);
    }
    #[cfg(not(windows))]
    asm.pop_reg(PhysReg::Rbp);
    #[cfg(not(windows))]
    asm.ret();
}

#[cfg(windows)]
fn is_xmm(reg: PhysReg) -> bool {
    matches!(
        reg,
        PhysReg::Xmm0
            | PhysReg::Xmm1
            | PhysReg::Xmm2
            | PhysReg::Xmm3
            | PhysReg::Xmm4
            | PhysReg::Xmm5
            | PhysReg::Xmm6
            | PhysReg::Xmm7
            | PhysReg::Xmm8
            | PhysReg::Xmm9
            | PhysReg::Xmm10
            | PhysReg::Xmm11
            | PhysReg::Xmm12
            | PhysReg::Xmm13
            | PhysReg::Xmm14
            | PhysReg::Xmm15
            | PhysReg::Xmm16
            | PhysReg::Xmm17
            | PhysReg::Xmm18
            | PhysReg::Xmm19
            | PhysReg::Xmm20
            | PhysReg::Xmm21
            | PhysReg::Xmm22
            | PhysReg::Xmm23
            | PhysReg::Xmm24
            | PhysReg::Xmm25
            | PhysReg::Xmm26
            | PhysReg::Xmm27
            | PhysReg::Xmm28
            | PhysReg::Xmm29
            | PhysReg::Xmm30
            | PhysReg::Xmm31
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Assembler;

    #[test]
    fn sysv_callee_saved_excludes_rbp() {
        assert_eq!(
            SYSV_CALLEE_SAVED,
            &[
                PhysReg::Rbx,
                PhysReg::R12,
                PhysReg::R13,
                PhysReg::R14,
                PhysReg::R15
            ]
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
