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
