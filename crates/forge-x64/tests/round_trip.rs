use forge_x64::{AluOp, Assembler, PhysReg};
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
