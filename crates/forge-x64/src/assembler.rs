use crate::PhysReg;

/// Emits x86-64 machine code byte by byte. The `Assembler` owns the
/// growing byte buffer and (starting in a later task) label/fixup state
/// for forward jump resolution.
pub struct Assembler {
    code: Vec<u8>,
}

impl Assembler {
    pub fn new() -> Self {
        Self { code: Vec::new() }
    }

    pub fn code(&self) -> &[u8] {
        &self.code
    }
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

/// The ModRM `mod` bits implied by a displacement value -- an enum rather
/// than a raw `u8` so that `emit_disp` below can match exhaustively with no
/// `unreachable!()` fallback arm, making a mismatched mode/displacement
/// pair (e.g. a `Disp8` mode paired with a value that doesn't fit)
/// structurally impossible to construct outside of `disp_mode` itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DispMode {
    /// mod=00: no displacement bytes.
    None,
    /// mod=01: one signed byte.
    Disp8,
    /// mod=10: four little-endian bytes.
    Disp32,
}

impl DispMode {
    /// The raw 2-bit ModRM `mod` field value.
    fn bits(self) -> u8 {
        match self {
            DispMode::None => 0b00,
            DispMode::Disp8 => 0b01,
            DispMode::Disp32 => 0b10,
        }
    }
}

/// Selects the smallest `DispMode` that can represent `disp`. This
/// function alone does not know about the rbp/r13-with-disp-0 trap
/// (mod=00 there would collide with RIP-relative addressing) -- callers
/// building a full ModRM/SIB byte are responsible for special-casing that
/// themselves.
fn disp_mode(disp: i32) -> DispMode {
    if disp == 0 {
        DispMode::None
    } else if i8::try_from(disp).is_ok() {
        DispMode::Disp8
    } else {
        DispMode::Disp32
    }
}

impl Assembler {
    /// Emits the displacement bytes implied by a `disp_mode` result.
    fn emit_disp(&mut self, mode: DispMode, disp: i32) {
        match mode {
            DispMode::None => {}
            DispMode::Disp8 => self.code.push(disp as i8 as u8),
            DispMode::Disp32 => self.code.extend_from_slice(&disp.to_le_bytes()),
        }
    }

    /// The REX prefix is the #1 source of subtle JIT bugs, because
    /// omitting it silently changes which register you addressed rather
    /// than failing. Three traps, all of which produce working-looking
    /// wrong code:
    ///   1. Without REX.W the operation is 32-bit and ZEROES the upper 32 bits.
    ///   2. Without REX.R/B you address rax-rdi instead of r8-r15.
    ///   3. With ANY REX prefix, byte registers spl/bpl/sil/dil replace
    ///      ah/ch/dh/bh -- silently different registers (not yet relevant
    ///      to this task, since no byte-register instructions exist yet).
    fn rex(&mut self, w: bool, reg: u8, index: u8, rm: u8) {
        let byte = 0x40
            | ((w as u8) << 3)
            | (((reg >> 3) & 1) << 2) // REX.R
            | (((index >> 3) & 1) << 1) // REX.X
            | ((rm >> 3) & 1); // REX.B
                               // Emit only when needed -- but ALWAYS when W, or when any register
                               // index is >= 8, or when addressing spl/bpl/sil/dil.
        if byte != 0x40 {
            self.code.push(byte);
        }
    }

    fn modrm_reg(&mut self, reg: u8, rm: u8) {
        self.code.push(0b11 << 6 | ((reg & 7) << 3) | (rm & 7));
    }

    /// `mov dst, src` -- register-to-register, 64-bit. Encoded as
    /// `REX.W + 89 /r` (MOV r/m64, r64): the ModRM.rm field is the
    /// destination and ModRM.reg is the source, matching the day-one
    /// spike's `48 89 F8` ("mov rax, rdi").
    pub fn mov_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        // index=0 is a placeholder: register-direct addressing has no
        // SIB/index operand, so REX.X is irrelevant here. Only matters once
        // modrm_mem/SIB with a real index register exists (Task 4).
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(0x89);
        self.modrm_reg(src.encoding(), dst.encoding());
    }

    /// Memory operand encoding, with three cases that MUST be
    /// special-cased:
    ///   * `base == RSP (4)`: ModRM.rm=100 means "SIB follows", so `[rsp]`
    ///     cannot be encoded directly -- a SIB byte with index=100 (none)
    ///     is required.
    ///   * `base == RBP (5)` with `disp == 0`: mod=00 rm=101 means
    ///     RIP-relative, NOT `[rbp]`. Must force mod=01 disp8=0.
    ///   * R12 and R13 hit the same two cases via REX.B -- very easy to
    ///     handle rsp/rbp and forget their extended twins.
    fn modrm_mem(&mut self, reg: u8, base: u8, disp: i32) {
        let base_low = base & 7;

        if base_low == 4 {
            // RSP or R12 -> SIB required
            let mode = disp_mode(disp);
            self.code.push(mode.bits() << 6 | ((reg & 7) << 3) | 0b100);
            self.code.push(0b00_100_100); // scale=1, index=none, base=rsp/r12
            self.emit_disp(mode, disp);
        } else if base_low == 5 && disp == 0 {
            // RBP or R13 -> must use disp8, mod=00 would mean RIP-relative
            self.code.push(0b01 << 6 | ((reg & 7) << 3) | base_low);
            self.code.push(0); // explicit zero displacement
        } else {
            let mode = disp_mode(disp);
            self.code
                .push(mode.bits() << 6 | ((reg & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }

    /// `mov dst, [base + disp]` -- 64-bit load. Encoded as
    /// `REX.W + 8B /r` (MOV r64, r/m64): ModRM.reg is the destination,
    /// ModRM.rm (via `modrm_mem`) addresses the memory operand.
    pub fn mov_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        // index=0 is a placeholder: this slice's memory operands are
        // base+disp only, with no index register, so REX.X is irrelevant
        // here. Out of scope per the design doc until a real SIB index
        // operand is added.
        self.rex(true, dst.encoding(), 0, base.encoding());
        self.code.push(0x8B);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_assembler_has_no_bytes() {
        let a = Assembler::new();
        assert_eq!(a.code(), &[] as &[u8]);
    }

    #[test]
    fn disp_mode_selects_none_for_zero() {
        assert_eq!(disp_mode(0), DispMode::None);
    }

    #[test]
    fn disp_mode_selects_disp8_for_values_fitting_in_i8() {
        assert_eq!(disp_mode(5), DispMode::Disp8);
        assert_eq!(disp_mode(-128), DispMode::Disp8);
        assert_eq!(disp_mode(127), DispMode::Disp8);
    }

    #[test]
    fn disp_mode_selects_disp32_for_values_not_fitting_in_i8() {
        assert_eq!(disp_mode(128), DispMode::Disp32);
        assert_eq!(disp_mode(-129), DispMode::Disp32);
        assert_eq!(disp_mode(1000), DispMode::Disp32);
        assert_eq!(disp_mode(i32::MAX), DispMode::Disp32);
        assert_eq!(disp_mode(i32::MIN), DispMode::Disp32);
    }

    #[test]
    fn emit_disp_none_emits_nothing() {
        let mut a = Assembler::new();
        a.emit_disp(DispMode::None, 0);
        assert_eq!(a.code(), &[] as &[u8]);
    }

    #[test]
    fn emit_disp_disp8_emits_one_byte() {
        let mut a = Assembler::new();
        a.emit_disp(DispMode::Disp8, -5);
        assert_eq!(a.code(), &[(-5i8) as u8]);
    }

    #[test]
    fn emit_disp_disp32_emits_four_bytes_little_endian() {
        let mut a = Assembler::new();
        a.emit_disp(DispMode::Disp32, 1000);
        assert_eq!(a.code(), &[0xE8, 0x03, 0x00, 0x00]);
    }

    #[test]
    fn emit_disp_disp32_handles_negative_values() {
        let mut a = Assembler::new();
        a.emit_disp(DispMode::Disp32, -1000);
        assert_eq!(a.code(), &(-1000i32).to_le_bytes());
    }

    #[test]
    fn rex_is_omitted_when_nothing_needs_it() {
        let mut a = Assembler::new();
        a.rex(false, 0, 0, 0);
        assert_eq!(a.code(), &[] as &[u8]);
    }

    #[test]
    fn rex_sets_r_and_b_together_when_both_operands_need_extension() {
        let mut a = Assembler::new();
        a.rex(true, 9, 0, 12); // reg=R9 (needs REX.R), rm=R12 (needs REX.B)
        assert_eq!(a.code(), &[0x4D]); // W=1, R=1, X=0, B=1 -> 0x40|0x08|0x04|0x01
    }

    #[test]
    fn modrm_reg_encodes_matching_reg_and_rm() {
        let mut a = Assembler::new();
        a.modrm_reg(0, 0); // reg==rm, e.g. the "rax,rax" case
        assert_eq!(a.code(), &[0xC0]);
    }
}
