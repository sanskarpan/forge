use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};

/// Decodes `bytes` as x86-64 machine code starting at address 0 and returns
/// one NASM-syntax mnemonic-and-operands string per instruction, in order.
/// Used to verify emitted code without needing to execute it (this repo's
/// dev machines may not be x86-64 hosts).
pub fn disassemble(bytes: &[u8]) -> Vec<String> {
    let mut decoder = Decoder::with_ip(64, bytes, 0, DecoderOptions::NONE);
    let mut formatter = NasmFormatter::new();
    let mut lines = Vec::new();
    let mut output = String::new();
    for instr in &mut decoder {
        output.clear();
        formatter.format(&instr, &mut output);
        lines.push(output.clone());
    }
    lines
}
