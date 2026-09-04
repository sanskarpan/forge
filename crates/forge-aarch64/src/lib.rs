//! Small, real AArch64 scalar encoder used as the foundation for the full
//! backend. Instructions are kept as 32-bit words until [`Assembler::bytes`]
//! serializes them in architectural little-endian order.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gpr(u8);

impl Gpr {
    pub const fn new(index: u8) -> Self {
        assert!(index < 31, "AArch64 GPR must be X0..X30");
        Self(index)
    }

    pub const fn index(self) -> u8 {
        self.0
    }
}

pub const SP: Gpr = Gpr(31);

#[derive(Default)]
pub struct Assembler {
    words: Vec<u32>,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn words(&self) -> &[u32] {
        &self.words
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    /// Emits `add Xd, Xn, #imm12` or its `sub` counterpart. The optional
    /// 12-bit left shift is encoded by setting the instruction's sh bit.
    pub fn add_imm(&mut self, dst: Gpr, src: Gpr, imm: u16, shift12: bool) {
        self.words
            .push(encode_add_sub_imm(false, dst, src, imm, shift12));
    }

    pub fn sub_imm(&mut self, dst: Gpr, src: Gpr, imm: u16, shift12: bool) {
        self.words
            .push(encode_add_sub_imm(true, dst, src, imm, shift12));
    }

    /// Emits the base-ISA `mul Xd, Xn, Xm` alias of `madd ... , XZR`.
    pub fn mul(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(
            0x9b00_7c00
                | (u32::from(rhs.index()) << 16)
                | (u32::from(lhs.index()) << 5)
                | u32::from(dst.index()),
        );
    }

    /// Emits `ret` (return through X30).
    pub fn ret(&mut self) {
        self.words.push(0xd65f_03c0);
    }
}

fn encode_add_sub_imm(sub: bool, dst: Gpr, src: Gpr, imm: u16, shift12: bool) -> u32 {
    assert!(imm < 4096, "AArch64 add/sub immediate must fit 12 bits");
    0x9100_0000
        | (u32::from(sub) << 30)
        | (u32::from(shift12) << 22)
        | (u32::from(imm) << 10)
        | (u32::from(src.index()) << 5)
        | u32::from(dst.index())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendInfo {
    pub target_available: bool,
    pub neon_available: bool,
}

pub const fn backend_info() -> BackendInfo {
    BackendInfo {
        target_available: cfg!(target_arch = "aarch64"),
        neon_available: cfg!(target_arch = "aarch64"),
    }
}

pub fn is_native_target() -> bool {
    backend_info().target_available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_fixed_width_little_endian_words() {
        let mut asm = Assembler::new();
        asm.add_imm(Gpr::new(0), Gpr::new(1), 7, false);
        asm.mul(Gpr::new(2), Gpr::new(0), Gpr::new(1));
        asm.ret();
        assert_eq!(asm.words(), &[0x9100_1c20, 0x9b01_7c02, 0xd65f_03c0]);
        assert_eq!(asm.bytes().len(), 12);
    }

    #[test]
    fn sub_immediate_sets_the_subtract_bit() {
        let mut asm = Assembler::new();
        asm.sub_imm(Gpr::new(0), Gpr::new(1), 1, false);
        assert_eq!(asm.words(), &[0xd100_0420]);
    }
}
