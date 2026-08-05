use forge_x64::{Assembler, PhysReg};
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
