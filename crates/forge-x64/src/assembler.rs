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

/// Returns the ModRM `mod` bits for a given displacement: `00` (no
/// displacement bytes), `01` (fits in a single signed byte), or `10`
/// (needs the full 32-bit form). This function alone does not know about
/// the rbp/r13-with-disp-0 trap (mod=00 there would collide with
/// RIP-relative addressing) -- callers building a full ModRM/SIB byte are
/// responsible for special-casing that themselves.
///
/// Not yet called from production code -- only from this module's tests --
/// until Task 4's `modrm_mem` wires it in, so `#[allow(dead_code)]` here is
/// temporary (same reasoning as the deferred `labels`/`fixups` fields
/// described above: `cargo clippy --workspace -- -D warnings` builds
/// without `#[cfg(test)]`, so it can't see the test-only call site).
#[allow(dead_code)]
fn disp_mode(disp: i32) -> u8 {
    if disp == 0 {
        0b00
    } else if i8::try_from(disp).is_ok() {
        0b01
    } else {
        0b10
    }
}

impl Assembler {
    /// Emits the displacement bytes implied by a `disp_mode` result: zero
    /// bytes for mode 00, one byte for mode 01, four little-endian bytes
    /// for mode 10.
    ///
    /// Same temporary situation as `disp_mode` above: only test-called
    /// until Task 4's `modrm_mem` uses it for real.
    #[allow(dead_code)]
    fn emit_disp(&mut self, mode: u8, disp: i32) {
        match mode {
            0b00 => {}
            0b01 => self.code.push(disp as i8 as u8),
            0b10 => self.code.extend_from_slice(&disp.to_le_bytes()),
            _ => unreachable!("disp_mode only ever returns 0b00, 0b01, or 0b10"),
        }
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
    fn disp_mode_selects_00_for_zero() {
        assert_eq!(disp_mode(0), 0b00);
    }

    #[test]
    fn disp_mode_selects_01_for_values_fitting_in_i8() {
        assert_eq!(disp_mode(5), 0b01);
        assert_eq!(disp_mode(-128), 0b01);
        assert_eq!(disp_mode(127), 0b01);
    }

    #[test]
    fn disp_mode_selects_10_for_values_not_fitting_in_i8() {
        assert_eq!(disp_mode(128), 0b10);
        assert_eq!(disp_mode(-129), 0b10);
        assert_eq!(disp_mode(1000), 0b10);
    }

    #[test]
    fn emit_disp_mode_00_emits_nothing() {
        let mut a = Assembler::new();
        a.emit_disp(0b00, 0);
        assert_eq!(a.code(), &[] as &[u8]);
    }

    #[test]
    fn emit_disp_mode_01_emits_one_byte() {
        let mut a = Assembler::new();
        a.emit_disp(0b01, -5);
        assert_eq!(a.code(), &[(-5i8) as u8]);
    }

    #[test]
    fn emit_disp_mode_10_emits_four_bytes_little_endian() {
        let mut a = Assembler::new();
        a.emit_disp(0b10, 1000);
        assert_eq!(a.code(), &[0xE8, 0x03, 0x00, 0x00]);
    }
}
