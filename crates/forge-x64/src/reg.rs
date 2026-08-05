/// A physical x86-64 register: all 16 general-purpose registers and all 32
/// XMM slots (XMM16-31 need EVEX to reach and can't be used by anything
/// built so far -- their encoding numbers are still real data worth
/// representing now, since gating actual usability is an AVX-512-era
/// concern, not a `PhysReg` concern).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysReg {
    Rax,
    Rcx,
    Rdx,
    Rbx,
    Rsp,
    Rbp,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    Xmm0,
    Xmm1,
    Xmm2,
    Xmm3,
    Xmm4,
    Xmm5,
    Xmm6,
    Xmm7,
    Xmm8,
    Xmm9,
    Xmm10,
    Xmm11,
    Xmm12,
    Xmm13,
    Xmm14,
    Xmm15,
    Xmm16,
    Xmm17,
    Xmm18,
    Xmm19,
    Xmm20,
    Xmm21,
    Xmm22,
    Xmm23,
    Xmm24,
    Xmm25,
    Xmm26,
    Xmm27,
    Xmm28,
    Xmm29,
    Xmm30,
    Xmm31,
}

impl PhysReg {
    /// The hardware encoding number: 0-15 for GPRs, 0-31 for XMM. GPRs and
    /// XMM registers share the same 0-15 (or 0-31) numbering space --
    /// distinguishing "GPR 0" from "XMM 0" is the caller's job (which
    /// opcode/ModRM.reg-or-rm slot this number goes into), not this type's.
    pub fn encoding(self) -> u8 {
        use PhysReg::*;
        match self {
            Rax => 0,
            Rcx => 1,
            Rdx => 2,
            Rbx => 3,
            Rsp => 4,
            Rbp => 5,
            Rsi => 6,
            Rdi => 7,
            R8 => 8,
            R9 => 9,
            R10 => 10,
            R11 => 11,
            R12 => 12,
            R13 => 13,
            R14 => 14,
            R15 => 15,
            Xmm0 => 0,
            Xmm1 => 1,
            Xmm2 => 2,
            Xmm3 => 3,
            Xmm4 => 4,
            Xmm5 => 5,
            Xmm6 => 6,
            Xmm7 => 7,
            Xmm8 => 8,
            Xmm9 => 9,
            Xmm10 => 10,
            Xmm11 => 11,
            Xmm12 => 12,
            Xmm13 => 13,
            Xmm14 => 14,
            Xmm15 => 15,
            Xmm16 => 16,
            Xmm17 => 17,
            Xmm18 => 18,
            Xmm19 => 19,
            Xmm20 => 20,
            Xmm21 => 21,
            Xmm22 => 22,
            Xmm23 => 23,
            Xmm24 => 24,
            Xmm25 => 25,
            Xmm26 => 26,
            Xmm27 => 27,
            Xmm28 => 28,
            Xmm29 => 29,
            Xmm30 => 30,
            Xmm31 => 31,
        }
    }

    /// Whether addressing this register on its own merits (independent of
    /// REX.W or any other operand) requires a REX prefix -- true for any
    /// encoding number >= 8 (r8-r15, xmm8-xmm31).
    pub fn needs_rex(self) -> bool {
        self.encoding() >= 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpr_encodings_match_hardware_numbers() {
        use PhysReg::*;
        let expected = [
            (Rax, 0),
            (Rcx, 1),
            (Rdx, 2),
            (Rbx, 3),
            (Rsp, 4),
            (Rbp, 5),
            (Rsi, 6),
            (Rdi, 7),
            (R8, 8),
            (R9, 9),
            (R10, 10),
            (R11, 11),
            (R12, 12),
            (R13, 13),
            (R14, 14),
            (R15, 15),
        ];
        for (reg, expected_encoding) in expected {
            assert_eq!(reg.encoding(), expected_encoding, "{reg:?}");
        }
    }

    #[test]
    fn xmm_encodings_match_hardware_numbers() {
        use PhysReg::*;
        let expected = [
            Xmm0, Xmm1, Xmm2, Xmm3, Xmm4, Xmm5, Xmm6, Xmm7, Xmm8, Xmm9, Xmm10, Xmm11, Xmm12, Xmm13,
            Xmm14, Xmm15, Xmm16, Xmm17, Xmm18, Xmm19, Xmm20, Xmm21, Xmm22, Xmm23, Xmm24, Xmm25,
            Xmm26, Xmm27, Xmm28, Xmm29, Xmm30, Xmm31,
        ];
        for (i, reg) in expected.into_iter().enumerate() {
            assert_eq!(reg.encoding(), i as u8, "{reg:?}");
        }
    }

    #[test]
    fn needs_rex_is_true_exactly_for_encoding_8_and_above() {
        assert!(!PhysReg::Rdi.needs_rex()); // encoding 7
        assert!(PhysReg::R8.needs_rex()); // encoding 8
        assert!(!PhysReg::Rax.needs_rex()); // encoding 0
        assert!(!PhysReg::Xmm7.needs_rex());
        assert!(PhysReg::Xmm8.needs_rex());
    }
}
