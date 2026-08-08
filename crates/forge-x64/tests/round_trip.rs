use forge_x64::{AluOp, Assembler, ConditionCode, PhysReg};
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, NasmFormatter};

/// Assembles into a fresh, disposable buffer and returns each decoded
/// instruction's formatted text, in order. This is the project's test
/// oracle for "did we encode what we meant to encode" -- see PROMPT.md's
/// rule that `iced-x86` never appears outside a test path (this whole file
/// is a `tests/` integration test binary, compiled separately from `src/`,
/// so that rule is structurally enforced, not just followed by convention).
fn disassemble(bytes: &[u8]) -> Vec<String> {
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

#[test]
fn mov_reg_reg_needs_rex_b_for_an_extended_destination() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::R12, PhysReg::Rax); // dst=r12 (needs REX.B), src=rax
    assert_eq!(a.code(), &[0x49, 0x89, 0xC4]);
    assert_eq!(disassemble(a.code()), vec!["mov r12,rax"]);
}

#[test]
fn mov_reg_reg_still_emits_rex_w_when_no_other_rex_bit_is_needed() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::Rbx, PhysReg::Rax); // neither register needs REX.R/X/B
    assert_eq!(a.code(), &[0x48, 0x89, 0xC3]);
    assert_eq!(disassemble(a.code()), vec!["mov rbx,rax"]);
}

#[test]
fn mov_reg_reg_needs_rex_r_for_an_extended_source() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::Rax, PhysReg::R9); // dst=rax, src=r9 (needs REX.R)
    assert_eq!(a.code(), &[0x4C, 0x89, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,r9"]);
}

#[test]
fn mov_reg_mem_generic_base_with_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rcx, 8);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x41, 0x08]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rcx+8]"]);
}

#[test]
fn mov_reg_mem_generic_base_with_disp32() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rcx, 1000);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x81, 0xE8, 0x03, 0x00, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rcx+3E8h]"]);
}

/// rsp requires a SIB byte -- ModRM.rm=100 alone means "SIB follows", so
/// `[rsp]` cannot be encoded without one.
#[test]
fn mov_reg_mem_rsp_base_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rsp, 0);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x04, 0x24]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rsp]"]);
}

/// r12 hits the SAME SIB-required case as rsp, via REX.B -- easy to
/// handle rsp and forget its extended twin.
#[test]
fn mov_reg_mem_r12_base_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R12, 0);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x04, 0x24]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r12]"]);
}

/// rbp with disp=0 must use mod=01 disp8=0 -- mod=00 rm=101 means
/// RIP-relative, not `[rbp]`.
#[test]
fn mov_reg_mem_rbp_base_with_zero_disp_forces_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rbp, 0);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x45, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rbp]"]);
}

/// r13 hits the SAME disp0 trap as rbp, via REX.B.
#[test]
fn mov_reg_mem_r13_base_with_zero_disp_forces_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R13, 0);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x45, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r13]"]);
}

/// rsp still needs its forced SIB byte even with a non-zero displacement --
/// the SIB requirement and the disp8/disp32 mode selection are independent,
/// so this checks the two layer correctly on top of each other.
#[test]
fn mov_reg_mem_rsp_base_with_disp8_still_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rsp, 8);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x44, 0x24, 0x08]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rsp+8]"]);
}

/// r12's SIB-required case with a disp32, via REX.B -- same layering check
/// as the rsp/disp8 case above, but with the extended twin and the other
/// displacement size.
#[test]
fn mov_reg_mem_r12_base_with_disp32_still_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R12, 1000);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x84, 0x24, 0xE8, 0x03, 0x00, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r12+3E8h]"]);
}

/// rbp with a NON-zero disp8 must NOT hit the forced-disp8-for-zero special
/// case -- it should fall through to the normal path and naturally select
/// disp8 via `disp_mode`, landing on the same bytes a generic base would.
#[test]
fn mov_reg_mem_rbp_base_with_nonzero_disp8_uses_normal_path() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rbp, 8);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x45, 0x08]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rbp+8]"]);
}

/// r13 with a non-zero disp32 hits the same normal-path point as rbp above,
/// via REX.B.
#[test]
fn mov_reg_mem_r13_base_with_nonzero_disp32_uses_normal_path() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R13, 1000);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x85, 0xE8, 0x03, 0x00, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r13+3E8h]"]);
}

#[test]
fn backward_jump_that_fits_uses_rel8() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler: 48 89 C0
    let len_before_jmp = a.code().len();
    a.jmp(l); // backward reference, close enough for rel8

    let expected_rel = -(len_before_jmp as i32 + 2); // rel8 measured from the end of this 2-byte instruction
    assert_eq!(a.code()[len_before_jmp], 0xEB);
    assert_eq!(a.code()[len_before_jmp + 1], expected_rel as i8 as u8);
    assert_eq!(a.code().len(), len_before_jmp + 2);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with("jmp"));
}

#[test]
fn backward_jump_that_does_not_fit_uses_rel32() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    for _ in 0..50 {
        a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes each, 150 bytes total -- far enough that rel8 can't reach
    }
    let len_before_jmp = a.code().len();
    a.jmp(l); // backward reference, too far for rel8

    let expected_rel = -(len_before_jmp as i32 + 5); // rel32 measured from the end of this 5-byte instruction
    assert_eq!(a.code()[len_before_jmp], 0xE9);
    assert_eq!(
        &a.code()[len_before_jmp + 1..len_before_jmp + 5],
        &expected_rel.to_le_bytes()
    );
    assert_eq!(a.code().len(), len_before_jmp + 5);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with("jmp"));
}

#[test]
fn forward_jump_always_uses_rel32() {
    let mut a = Assembler::new();
    let l = a.new_label();
    let jmp_at = a.code().len(); // 0
    a.jmp(l); // forward reference -- label not bound yet
    assert_eq!(a.code()[jmp_at], 0xE9); // always rel32 for forward jumps, never rel8
    assert_eq!(a.code().len(), jmp_at + 5);

    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler between the jmp and its target
    let target_pos = a.code().len();
    a.bind(l); // resolves the fixup recorded above

    let expected_rel = target_pos as i32 - (jmp_at as i32 + 5); // rel32 measured from the end of the 5-byte jmp
    assert_eq!(
        &a.code()[jmp_at + 1..jmp_at + 5],
        &expected_rel.to_le_bytes()
    );

    let text = disassemble(a.code());
    assert!(text[0].starts_with("jmp"));
}

/// `bind()` walks `fixups` and removes entries in place while iterating --
/// an off-by-one there (e.g. incrementing the index after a `Vec::remove`)
/// would silently skip patching the fixup that shifted into the removed
/// slot. Pin this down with two pending forward references to the same
/// label before it's bound.
#[test]
fn binding_a_label_resolves_all_pending_forward_fixups() {
    let mut a = Assembler::new();
    let l = a.new_label();
    let first_jmp_at = a.code().len();
    a.jmp(l); // forward reference #1
    let second_jmp_at = a.code().len();
    a.jmp(l); // forward reference #2
    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // filler so the second fixup's
                                               // correct patched value is
                                               // nonzero, not coincidentally
                                               // identical to the unpatched
                                               // placeholder bytes
    let target_pos = a.code().len();
    a.bind(l); // must resolve BOTH pending fixups

    let expected_rel_first = target_pos as i32 - (first_jmp_at as i32 + 5);
    let expected_rel_second = target_pos as i32 - (second_jmp_at as i32 + 5);
    assert_eq!(
        &a.code()[first_jmp_at + 1..first_jmp_at + 5],
        &expected_rel_first.to_le_bytes()
    );
    assert_eq!(
        &a.code()[second_jmp_at + 1..second_jmp_at + 5],
        &expected_rel_second.to_le_bytes()
    );
}

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
    // Verified empirically: iced-x86 renders the sign-extended 64-bit
    // immediate as its hex pattern, not as decimal "-1".
    assert_eq!(disassemble(a.code()), vec!["sub rbx,0FFFFFFFFFFFFFFFFh"]);
}

#[test]
fn alu_reg_imm_and_falls_back_to_imm32_when_it_does_not_fit_in_i8() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::And, PhysReg::Rax, 1000);
    assert_eq!(a.code(), &[0x48, 0x81, 0xE0, 0xE8, 0x03, 0x00, 0x00]);
    // Verified empirically: same hex-not-decimal rendering 6a's plan found
    // for displacements applies to immediates too.
    assert_eq!(disassemble(a.code()), vec!["and rax,3E8h"]);
}

/// Or and Xor's r/imm forms aren't covered by the three tests above (which
/// only exercise Add/Sub/And) -- their opcode-extension digits (/1 and /6)
/// are otherwise never checked through a real disassembler in the
/// immediate form, so a transposition in `AluOp::extension()` for either
/// one would go uncaught (their r/r opcodes are completely different
/// bytes, so `alu_reg_reg`'s tests don't cover this).
#[test]
fn alu_reg_imm_or() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::Or, PhysReg::Rax, 5);
    assert_eq!(a.code(), &[0x48, 0x83, 0xC8, 0x05]);
    assert_eq!(disassemble(a.code()), vec!["or rax,5"]);
}

#[test]
fn alu_reg_imm_xor() {
    let mut a = Assembler::new();
    a.alu_reg_imm(AluOp::Xor, PhysReg::Rax, 5);
    assert_eq!(a.code(), &[0x48, 0x83, 0xF0, 0x05]);
    assert_eq!(disassemble(a.code()), vec!["xor rax,5"]);
}

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
    // Verified empirically: iced-x86 renders the immediate operand in hex,
    // same convention already found for alu_reg_imm's immediates.
    assert_eq!(disassemble(a.code()), vec!["imul rax,rbx,0Ah"]);
}

#[test]
fn mov_reg_imm_uses_the_compact_form_when_the_value_fits_in_i32() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, 42);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC0, 0x2A, 0x00, 0x00, 0x00]);
    // Verified empirically: iced-x86 renders the immediate in hex ("2Ah"),
    // not decimal, same convention already found for alu_reg_imm.
    assert_eq!(disassemble(a.code()), vec!["mov rax,2Ah"]);
}

#[test]
fn mov_reg_imm_compact_form_handles_a_negative_value() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rbx, -1);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC3, 0xFF, 0xFF, 0xFF, 0xFF]);
    // Verified empirically: iced-x86 renders the sign-extended 64-bit
    // immediate as its hex pattern, not as decimal "-1" -- same finding as
    // alu_reg_imm_sub_imm8_handles_a_negative_value.
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
    // Verified empirically: iced-x86's NasmFormatter names this mnemonic
    // "mov", not "movabs", even for the B8+rd/REX.W imm64 form.
    assert_eq!(disassemble(a.code()), vec!["mov rax,7FFFFFFFFFFFFFFFh"]);
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
    // Verified empirically: same "mov", not "movabs", naming as above.
    assert_eq!(disassemble(a.code()), vec!["mov r9,7FFFFFFFFFFFFFFFh"]);
}

/// `mov_reg_imm`'s `i32::try_from(value)` (i64 -> i32) is the only
/// conversion of this shape anywhere in the crate -- unlike
/// `alu_reg_imm`'s i32->i8 conversion, which is exercised near its
/// boundary indirectly via `disp_mode`'s i32::MAX/MIN/128/-129 tests,
/// nothing else here would catch an off-by-one at this specific boundary.
/// This test and the three below pin all four edges: MAX/MIN exactly (both
/// still compact) and one past each (both must switch to movabs).
#[test]
fn mov_reg_imm_uses_compact_form_at_the_i32_max_boundary() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, i32::MAX as i64);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0x7F]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,7FFFFFFFh"]);
}

#[test]
fn mov_reg_imm_uses_movabs_just_past_the_i32_max_boundary() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, i32::MAX as i64 + 1);
    assert_eq!(
        a.code(),
        &[0x48, 0xB8, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00]
    );
    assert_eq!(disassemble(a.code()), vec!["mov rax,80000000h"]);
}

#[test]
fn mov_reg_imm_uses_compact_form_at_the_i32_min_boundary() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, i32::MIN as i64);
    assert_eq!(a.code(), &[0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x80]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,0FFFFFFFF80000000h"]);
}

#[test]
fn mov_reg_imm_uses_movabs_just_past_the_i32_min_boundary() {
    let mut a = Assembler::new();
    a.mov_reg_imm(PhysReg::Rax, i32::MIN as i64 - 1);
    assert_eq!(
        a.code(),
        &[0x48, 0xB8, 0xFF, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    assert_eq!(disassemble(a.code()), vec!["mov rax,0FFFFFFFF7FFFFFFFh"]);
}

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
    // Verified empirically: iced-x86 renders the immediate in hex ("3E8h"),
    // not decimal, same convention already found for alu_reg_imm/mov_reg_imm.
    assert_eq!(disassemble(a.code()), vec!["test rax,3E8h"]);
}

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
    assert_eq!(
        &a.code()[jcc_at + 2..jcc_at + 6],
        &expected_rel.to_le_bytes()
    );

    let text = disassemble(a.code());
    assert!(text[0].starts_with('j'));
}

#[test]
fn not_reg_flips_all_bits() {
    let mut a = Assembler::new();
    a.not_reg(PhysReg::Rax);
    assert_eq!(a.code(), &[0x48, 0xF7, 0xD0]);
    assert_eq!(disassemble(a.code()), vec!["not rax"]);
}

#[test]
fn not_reg_with_extended_register_sets_rex_b() {
    let mut a = Assembler::new();
    a.not_reg(PhysReg::R8);
    assert_eq!(a.code(), &[0x49, 0xF7, 0xD0]);
    assert_eq!(disassemble(a.code()), vec!["not r8"]);
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
