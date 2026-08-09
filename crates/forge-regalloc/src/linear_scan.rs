use forge_x64::PhysReg;

/// A virtual register's final storage location, once Phase 8 has assigned
/// one. SPEC.md's §7 pseudocode references `Location` but never defines
/// it -- defined here, since this is the first slice needing it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Location {
    Reg(PhysReg),
    /// Stack slot index. Phase 8c's concern entirely -- this slice never
    /// constructs this variant; it exists now only so `Location`'s shape
    /// is settled before 8c needs to extend the same enum.
    Spill(u32),
}

/// System V AMD64 GPRs available for allocation: all 16 minus `Rsp`
/// (stack pointer, never a virtual register's home) and `Rbp` (frame
/// pointer, same reasoning as `prologue::SYSV_CALLEE_SAVED` already
/// excluding it).
pub const ALLOCATABLE_GPR: &[PhysReg] = &[
    PhysReg::Rax,
    PhysReg::Rcx,
    PhysReg::Rdx,
    PhysReg::Rbx,
    PhysReg::Rsi,
    PhysReg::Rdi,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R10,
    PhysReg::R11,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

/// XMM registers available for allocation: Xmm0-15 only. Xmm16-31 need
/// EVEX to reach and nothing in this codebase can encode an
/// EVEX-prefixed instruction yet, so handing one out would produce
/// unencodable output.
pub const ALLOCATABLE_XMM: &[PhysReg] = &[
    PhysReg::Xmm0,
    PhysReg::Xmm1,
    PhysReg::Xmm2,
    PhysReg::Xmm3,
    PhysReg::Xmm4,
    PhysReg::Xmm5,
    PhysReg::Xmm6,
    PhysReg::Xmm7,
    PhysReg::Xmm8,
    PhysReg::Xmm9,
    PhysReg::Xmm10,
    PhysReg::Xmm11,
    PhysReg::Xmm12,
    PhysReg::Xmm13,
    PhysReg::Xmm14,
    PhysReg::Xmm15,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phys_reg_hash_derive_works() {
        // Confirms the derive actually compiles and behaves correctly --
        // cheap, but real, since a missing/broken derive would otherwise
        // only surface as a confusing downstream compile error far from
        // its cause.
        let set: std::collections::HashSet<PhysReg> =
            [PhysReg::Rax, PhysReg::Rax].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn allocatable_gpr_excludes_rsp_and_rbp() {
        assert_eq!(ALLOCATABLE_GPR.len(), 14);
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rsp));
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rbp));
    }

    #[test]
    fn allocatable_xmm_excludes_xmm16_through_31() {
        assert_eq!(ALLOCATABLE_XMM.len(), 16);
        for r in ALLOCATABLE_XMM {
            assert!(
                r.encoding() < 16,
                "{r:?} has encoding >= 16, unencodable without EVEX"
            );
        }
    }

    #[test]
    fn location_reg_and_spill_are_distinct_and_comparable() {
        assert_ne!(Location::Reg(PhysReg::Rax), Location::Spill(0));
        assert_eq!(Location::Reg(PhysReg::Rax), Location::Reg(PhysReg::Rax));
    }
}
