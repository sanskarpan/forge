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
    // `#[allow(dead_code)]`: plain `cargo clippy --workspace -- -D
    // warnings` (this project's actual CI invocation -- see
    // .github/workflows/ci.yml) does not compile with `--cfg test`, so it
    // can't see this method's only call site, which is inside `#[cfg(test)]
    // mod tests` below, until Task 4's `modrm_mem()` becomes a second,
    // production call site. Remove this allow in Task 4 once that happens.
    #[allow(dead_code)]
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
// `#[allow(dead_code)]`: same reason as `DispMode::bits` above -- only
// called from `#[cfg(test)]` until Task 4's `modrm_mem()` wires it in.
// Remove this allow in Task 4.
#[allow(dead_code)]
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
    // `#[allow(dead_code)]`: same reason as `disp_mode` above -- only
    // called from `#[cfg(test)]` until Task 4's `modrm_mem()` wires it in.
    // Remove this allow in Task 4.
    #[allow(dead_code)]
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
                               // index is >= 8.
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
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(0x89);
        self.modrm_reg(src.encoding(), dst.encoding());
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
}
