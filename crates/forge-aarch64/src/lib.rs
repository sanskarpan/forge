//! Small, real AArch64 scalar encoder used as the foundation for the full
//! backend. Instructions are kept as 32-bit words until [`Assembler::bytes`]
//! serializes them in architectural little-endian order.

use forge_ir::{CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

unsafe extern "C" {
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn exp(x: f64) -> f64;
    fn log(x: f64) -> f64;
    fn pow(x: f64, y: f64) -> f64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gpr(u8);

impl Gpr {
    pub const fn new(index: u8) -> Self {
        assert!(index < 31, "AArch64 GPR must be X0..X30");
        Self(index)
    }

    /// Constructs a register number for an AArch64 floating-point register.
    /// D0..D31 have 32 architectural registers, while the integer X register
    /// namespace reserves number 31 for SP/XZR and is therefore handled by
    /// [`Gpr::new`] plus the named constants below.
    pub const fn new_d(index: u8) -> Self {
        assert!(index < 32, "AArch64 D register must be D0..D31");
        Self(index)
    }

    pub const fn index(self) -> u8 {
        self.0
    }
}

pub const SP: Gpr = Gpr(31);

/// The architectural zero register. Register number 31 is interpreted as
/// `XZR` by arithmetic/logical register instructions and as `SP` by the
/// addressing and add/sub-immediate forms; keep the two names distinct at
/// call sites so an integer negation cannot accidentally read the stack
/// pointer as its zero operand.
pub const XZR: Gpr = Gpr(31);

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

    pub fn add_reg(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(add_reg(dst, lhs, rhs));
    }

    pub fn sub_reg(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(sub_reg(dst, lhs, rhs));
    }

    /// Emits the base-ISA `mul Xd, Xn, Xm` alias of `madd ... , XZR`.
    pub fn mul(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(mul(dst, lhs, rhs));
    }

    pub fn sdiv(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(sdiv(dst, lhs, rhs));
    }

    pub fn madd(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr, addend: Gpr) {
        self.words.push(madd(dst, lhs, rhs, addend));
    }

    pub fn msub(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr, subtrahend: Gpr) {
        self.words.push(msub(dst, lhs, rhs, subtrahend));
    }

    pub fn and_reg(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(and_reg(dst, lhs, rhs));
    }

    pub fn orr_reg(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(orr_reg(dst, lhs, rhs));
    }

    pub fn eor_reg(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(eor_reg(dst, lhs, rhs));
    }

    pub fn lsl(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(lsl(dst, lhs, rhs));
    }

    pub fn lsr(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(lsr(dst, lhs, rhs));
    }

    pub fn asr(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(asr(dst, lhs, rhs));
    }

    pub fn fadd_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fadd_d(dst, lhs, rhs));
    }

    pub fn fsub_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fsub_d(dst, lhs, rhs));
    }

    pub fn fmul_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fmul_d(dst, lhs, rhs));
    }

    pub fn fdiv_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fdiv_d(dst, lhs, rhs));
    }

    pub fn fsqrt_d(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(fsqrt_d(dst, src));
    }

    pub fn fabs_d(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(fabs_d(dst, src));
    }

    pub fn fneg_d(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(fneg_d(dst, src));
    }

    pub fn fmadd_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr, addend: Gpr) {
        self.words.push(fmadd_d(dst, lhs, rhs, addend));
    }

    pub fn fmsub_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr, subtrahend: Gpr) {
        self.words.push(fmsub_d(dst, lhs, rhs, subtrahend));
    }

    pub fn fmin_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fmin_d(dst, lhs, rhs));
    }

    pub fn fmax_d(&mut self, dst: Gpr, lhs: Gpr, rhs: Gpr) {
        self.words.push(fmax_d(dst, lhs, rhs));
    }

    pub fn fcvtzs(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(fcvtzs(dst, src));
    }

    pub fn scvtf(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(scvtf(dst, src));
    }

    pub fn ldr(&mut self, dst: Gpr, base: Gpr, offset_bytes: u16) {
        self.words.push(ldr(dst, base, offset_bytes));
    }

    pub fn str(&mut self, src: Gpr, base: Gpr, offset_bytes: u16) {
        self.words.push(str_(src, base, offset_bytes));
    }

    /// Emits `ldr Dd, [Xn, #offset]` for an unsigned, eight-byte-scaled
    /// scalar-double stack slot.
    pub fn ldr_d(&mut self, dst: Gpr, base: Gpr, offset_bytes: u16) {
        self.words.push(ldr_d(dst, base, offset_bytes));
    }

    /// Emits `str Dd, [Xn, #offset]` for an unsigned, eight-byte-scaled
    /// scalar-double stack slot.
    pub fn str_d(&mut self, src: Gpr, base: Gpr, offset_bytes: u16) {
        self.words.push(str_d(src, base, offset_bytes));
    }

    pub fn movz(&mut self, dst: Gpr, imm: u16, shift: u8) {
        self.words.push(movz(dst, imm, shift));
    }

    pub fn movk(&mut self, dst: Gpr, imm: u16, shift: u8) {
        self.words.push(movk(dst, imm, shift));
    }

    /// Emits `and dst, lhs, #imm` when `value` has an encodable bitmask
    /// pattern. Returns false when callers must materialize the constant.
    pub fn and_imm(&mut self, dst: Gpr, lhs: Gpr, value: u64) -> bool {
        let Some((n, immr, imms)) = encode_logical_imm(value, true) else {
            return false;
        };
        self.words.push(
            0x9200_0000
                | (u32::from(n) << 22)
                | (u32::from(immr) << 16)
                | (u32::from(imms) << 10)
                | (u32::from(lhs.index()) << 5)
                | u32::from(dst.index()),
        );
        true
    }

    /// Branch offsets are signed byte offsets from the branch instruction and
    /// must be four-byte aligned. Labels/fixups belong to the higher-level
    /// AArch64 backend; this primitive is useful for already-laid-out code.
    pub fn b(&mut self, offset_bytes: i32) {
        self.words.push(encode_branch(offset_bytes));
    }

    pub fn bl(&mut self, offset_bytes: i32) {
        self.words.push(encode_branch_link(offset_bytes));
    }

    /// Emits `blr Xn`, an indirect branch-with-link used for process-local
    /// libm calls whose absolute address cannot be reached by a direct `bl`.
    pub fn blr(&mut self, target: Gpr) {
        self.words.push(blr(target));
    }

    pub fn b_cond(&mut self, condition: Condition, offset_bytes: i32) {
        self.words.push(encode_branch_cond(condition, offset_bytes));
    }

    /// Emits `ret` (return through X30).
    pub fn ret(&mut self) {
        self.words.push(0xd65f_03c0);
    }

    /// Emits `ldr Dd, <literal>` with a signed byte offset from the current
    /// instruction. Literal-pool placement and range validation belong to the
    /// higher-level emitter.
    pub fn ldr_literal_d(&mut self, dst: Gpr, offset_bytes: i32) {
        self.words.push(encode_ldr_literal_d(dst, offset_bytes));
    }

    /// Emits `fmov Dd, Dn`.
    pub fn fmov_d(&mut self, dst: Gpr, src: Gpr) {
        self.words.push(fmov_d(dst, src));
    }

    /// Emits `fcmp Dn, Dm`, which writes the AArch64 condition flags.
    pub fn fcmp_d(&mut self, lhs: Gpr, rhs: Gpr) {
        self.words.push(fcmp_d(lhs, rhs));
    }

    /// Emits `cmp Xn, Xm` (`subs XZR, Xn, Xm`), which writes the integer
    /// condition flags used by the conditional branch forms.
    pub fn cmp_reg(&mut self, lhs: Gpr, rhs: Gpr) {
        self.words.push(cmp_reg(lhs, rhs));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Condition {
    Eq = 0,
    Ne = 1,
    Lt = 0xb,
    Ge = 0xa,
    Gt = 0xc,
    Le = 0xd,
    Al = 0xe,
}

fn rr(base: u32, dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    base | (u32::from(rhs.index()) << 16) | (u32::from(lhs.index()) << 5) | u32::from(dst.index())
}

pub fn add_reg(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x8b00_0000, dst, lhs, rhs)
}

pub fn sub_reg(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0xcb00_0000, dst, lhs, rhs)
}

pub fn mul(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x9b00_7c00, dst, lhs, rhs)
}

pub fn sdiv(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x9ac0_0c00, dst, lhs, rhs)
}

pub fn madd(dst: Gpr, lhs: Gpr, rhs: Gpr, addend: Gpr) -> u32 {
    0x9b00_0000
        | (u32::from(rhs.index()) << 16)
        | (u32::from(addend.index()) << 10)
        | (u32::from(lhs.index()) << 5)
        | u32::from(dst.index())
}

pub fn msub(dst: Gpr, lhs: Gpr, rhs: Gpr, subtrahend: Gpr) -> u32 {
    0x9b00_8000
        | (u32::from(rhs.index()) << 16)
        | (u32::from(subtrahend.index()) << 10)
        | (u32::from(lhs.index()) << 5)
        | u32::from(dst.index())
}

pub fn and_reg(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x8a00_0000, dst, lhs, rhs)
}

pub fn orr_reg(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0xaa00_0000, dst, lhs, rhs)
}

pub fn eor_reg(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0xca00_0000, dst, lhs, rhs)
}

pub fn lsl(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x9ac0_2000, dst, lhs, rhs)
}

pub fn lsr(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x9ac0_2400, dst, lhs, rhs)
}

pub fn asr(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x9ac0_2800, dst, lhs, rhs)
}

pub fn fadd_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x1e60_2800, dst, lhs, rhs)
}

pub fn fsub_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x1e60_3800, dst, lhs, rhs)
}

pub fn fmul_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x1e60_0800, dst, lhs, rhs)
}

pub fn fdiv_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x1e60_1800, dst, lhs, rhs)
}

pub fn fsqrt_d(dst: Gpr, src: Gpr) -> u32 {
    0x1e61_c000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn fabs_d(dst: Gpr, src: Gpr) -> u32 {
    0x1e60_c000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn fneg_d(dst: Gpr, src: Gpr) -> u32 {
    0x1e61_4000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn fmov_d(dst: Gpr, src: Gpr) -> u32 {
    0x1e60_4000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn fcmp_d(lhs: Gpr, rhs: Gpr) -> u32 {
    0x1e60_2000 | (u32::from(rhs.index()) << 16) | (u32::from(lhs.index()) << 5)
}

pub fn cmp_reg(lhs: Gpr, rhs: Gpr) -> u32 {
    0xeb00_001f | (u32::from(rhs.index()) << 16) | (u32::from(lhs.index()) << 5)
}

pub fn blr(target: Gpr) -> u32 {
    0xd63f_0000 | (u32::from(target.index()) << 5)
}

/// Resolves a libm symbol for an AArch64 process-local indirect call. The
/// resulting address is suitable for materializing with MOVZ/MOVK, but is not
/// serializable as a portable artifact; callers exporting artifacts reject
/// host libm calls before invoking this emitter.
pub fn libm_address(func: forge_ir::LibFunc) -> usize {
    type Unary = unsafe extern "C" fn(f64) -> f64;
    type Binary = unsafe extern "C" fn(f64, f64) -> f64;
    match func {
        forge_ir::LibFunc::Sin => sin as Unary as usize,
        forge_ir::LibFunc::Cos => cos as Unary as usize,
        forge_ir::LibFunc::Tan => tan as Unary as usize,
        forge_ir::LibFunc::Exp => exp as Unary as usize,
        forge_ir::LibFunc::Log => log as Unary as usize,
        forge_ir::LibFunc::Pow => pow as Binary as usize,
    }
}

pub fn fmadd_d(dst: Gpr, lhs: Gpr, rhs: Gpr, addend: Gpr) -> u32 {
    0x1f40_0000
        | (u32::from(rhs.index()) << 16)
        | (u32::from(addend.index()) << 10)
        | (u32::from(lhs.index()) << 5)
        | u32::from(dst.index())
}

pub fn fmsub_d(dst: Gpr, lhs: Gpr, rhs: Gpr, subtrahend: Gpr) -> u32 {
    0x1f40_8000
        | (u32::from(rhs.index()) << 16)
        | (u32::from(subtrahend.index()) << 10)
        | (u32::from(lhs.index()) << 5)
        | u32::from(dst.index())
}

pub fn fmin_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    // Forge's language-level min ignores a NaN when the other operand is a
    // number, matching the interpreter. AArch64's FMIN propagates NaNs;
    // FMINNM is the matching "numeric minimum" operation.
    rr(0x1e60_7800, dst, lhs, rhs)
}

pub fn fmax_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    // See fmin_d: use the numeric form so native execution preserves the
    // interpreter's NaN behavior.
    rr(0x1e60_6800, dst, lhs, rhs)
}

pub fn fcvtzs(dst: Gpr, src: Gpr) -> u32 {
    0x9e78_0000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn scvtf(dst: Gpr, src: Gpr) -> u32 {
    0x9e62_0000 | (u32::from(src.index()) << 5) | u32::from(dst.index())
}

pub fn ldr(dst: Gpr, base: Gpr, offset_bytes: u16) -> u32 {
    assert!(offset_bytes.is_multiple_of(8) && offset_bytes / 8 < 4096);
    0xf940_0000
        | (u32::from(offset_bytes / 8) << 10)
        | (u32::from(base.index()) << 5)
        | u32::from(dst.index())
}

pub fn str_(src: Gpr, base: Gpr, offset_bytes: u16) -> u32 {
    assert!(offset_bytes.is_multiple_of(8) && offset_bytes / 8 < 4096);
    0xf900_0000
        | (u32::from(offset_bytes / 8) << 10)
        | (u32::from(base.index()) << 5)
        | u32::from(src.index())
}

pub fn ldr_d(dst: Gpr, base: Gpr, offset_bytes: u16) -> u32 {
    assert!(offset_bytes.is_multiple_of(8) && offset_bytes / 8 < 4096);
    0xfd40_0000
        | (u32::from(offset_bytes / 8) << 10)
        | (u32::from(base.index()) << 5)
        | u32::from(dst.index())
}

pub fn str_d(src: Gpr, base: Gpr, offset_bytes: u16) -> u32 {
    assert!(offset_bytes.is_multiple_of(8) && offset_bytes / 8 < 4096);
    0xfd00_0000
        | (u32::from(offset_bytes / 8) << 10)
        | (u32::from(base.index()) << 5)
        | u32::from(src.index())
}

pub fn movz(dst: Gpr, imm: u16, shift: u8) -> u32 {
    assert!(shift.is_multiple_of(16) && shift <= 48);
    0xd280_0000 | (u32::from(shift / 16) << 21) | (u32::from(imm) << 5) | u32::from(dst.index())
}

pub fn movk(dst: Gpr, imm: u16, shift: u8) -> u32 {
    assert!(shift.is_multiple_of(16) && shift <= 48);
    0xf280_0000 | (u32::from(shift / 16) << 21) | (u32::from(imm) << 5) | u32::from(dst.index())
}

pub fn encode_branch(offset_bytes: i32) -> u32 {
    assert!(offset_bytes % 4 == 0);
    let imm = offset_bytes / 4;
    assert!((-(1 << 25)..(1 << 25)).contains(&imm));
    0x1400_0000 | ((imm as u32) & 0x03ff_ffff)
}

pub fn encode_branch_link(offset_bytes: i32) -> u32 {
    encode_branch(offset_bytes) | 0x8000_0000
}

pub fn encode_branch_cond(condition: Condition, offset_bytes: i32) -> u32 {
    assert!(offset_bytes % 4 == 0);
    let imm = offset_bytes / 4;
    assert!((-(1 << 18)..(1 << 18)).contains(&imm));
    0x5400_0000 | (((imm as u32) & 0x7ffff) << 5) | u32::from(condition as u8)
}

fn encode_ldr_literal_d(dst: Gpr, offset_bytes: i32) -> u32 {
    assert!(
        offset_bytes % 4 == 0,
        "AArch64 literal offset must be 4-byte aligned"
    );
    let words = offset_bytes / 4;
    assert!(
        (-0x40000..=0x3ffff).contains(&words),
        "AArch64 literal offset is outside the signed imm19 range"
    );
    0x5c00_0000 | (((words as u32) & 0x7ffff) << 5) | u32::from(dst.index())
}

/// Encodes the AArch64 logical-immediate pattern as `(N, immr, imms)`.
/// The search is tiny (six element widths and at most 64 rotations) and is
/// easier to audit than a table of special cases. It rejects the all-zero and
/// all-one patterns, which are architecturally unencodable.
pub fn encode_logical_imm(value: u64, sf: bool) -> Option<(u8, u8, u8)> {
    let max_width = if sf { 64 } else { 32 };
    for width in [2u32, 4, 8, 16, 32, 64] {
        if width > max_width {
            continue;
        }
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let pattern = value & mask;
        if pattern == 0 || pattern == mask {
            continue;
        }
        let mut repeated = 0u64;
        let mut shift = 0;
        while shift < max_width {
            repeated |= pattern << shift;
            shift += width;
        }
        if (if sf {
            value
        } else {
            value & u64::from(u32::MAX)
        }) != repeated
        {
            continue;
        }
        for ones in 1..width {
            let base = (1u64 << ones) - 1;
            for rotation in 0..width {
                let rotated = rotate_right(base, rotation, width);
                if rotated == pattern {
                    let n = u8::from(width == 64);
                    let imms = (((0x3f ^ (width - 1)) | (ones - 1)) & 0x3f) as u8;
                    return Some((n, rotation as u8, imms));
                }
            }
        }
    }
    None
}

fn rotate_right(value: u64, amount: u32, width: u32) -> u64 {
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    if amount == 0 {
        return value & mask;
    }
    ((value >> amount) | (value << (width - amount))) & mask
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

/// Emits a complete AAPCS64 scalar function with an f64 result for the
/// supported IR subset. Floating parameters use D0..D7 and integer/bool
/// parameters use X0..X7, as required by the independent AAPCS64 argument
/// banks. Temporaries use the corresponding register number in their class;
/// constants are loaded from an aligned literal pool or materialized with
/// MOVZ/MOVK. Branch edges materialize typed SSA φ values before transfer.
/// Temporaries use caller-saved D16..D31 or X8..X18 registers first. If those
/// are exhausted, the emitter allocates D8..D15 or X19..X28 and preserves the
/// selected callee-saved registers in a 16-byte-aligned stack frame.
pub fn emit_f64(function: &Function) -> Result<Vec<u8>, String> {
    if function.blocks.is_empty() {
        return Err("AArch64 emitter requires at least one block".to_string());
    }
    if function.types.last() != Some(&Ty::F64) {
        return Err("AArch64 mixed scalar emitter requires an f64 result".to_string());
    }

    let stack_spill_f64 = function.params.iter().all(|(_, ty)| *ty == Ty::F64)
        && function.blocks.iter().all(|block| {
            let instructions_supported = block.insts.iter().all(|value| {
                matches!(
                    function.insts.get(value.0 as usize),
                    Some(
                        Inst::ConstF64(_)
                            | Inst::Param { ty: Ty::F64, .. }
                            | Inst::Add(_, _)
                            | Inst::Sub(_, _)
                            | Inst::Mul(_, _)
                            | Inst::Div(_, _)
                            | Inst::Neg(_)
                            | Inst::Abs(_)
                            | Inst::Sqrt(_)
                            | Inst::Fma { .. }
                            | Inst::Min(_, _)
                            | Inst::Max(_, _)
                            | Inst::Cmp { .. }
                            | Inst::Phi { .. }
                            | Inst::Call { .. }
                    )
                )
            });
            let terminator_supported = match block.term.as_ref() {
                Some(Terminator::Return(result)) => {
                    function.types.get(result.0 as usize) == Some(&Ty::F64)
                }
                Some(Terminator::Jump(_)) => true,
                Some(Terminator::Branch { cond, .. }) => {
                    matches!(function.insts.get(cond.0 as usize), Some(Inst::Cmp { .. }))
                        && function.types.get(cond.0 as usize) == Some(&Ty::Bool)
                }
                None => false,
            };
            instructions_supported && terminator_supported
        });
    let non_param_count = function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter(|value| {
            !matches!(
                function.insts.get(value.0 as usize),
                Some(Inst::Param { .. })
            )
        })
        .count();
    let contains_f64_call = function
        .insts
        .iter()
        .any(|inst| matches!(inst, Inst::Call { .. }))
        && function.params.iter().all(|(_, ty)| *ty == Ty::F64);
    if stack_spill_f64 && (non_param_count > 24 || contains_f64_call) {
        return emit_f64_with_stack_spills(function);
    }

    let mixed_stack_spill_f64 = function.blocks.iter().all(|block| {
        let instructions_supported = block.insts.iter().all(|value| {
            matches!(
                function.insts.get(value.0 as usize),
                Some(
                    Inst::ConstF64(_)
                        | Inst::ConstI64(_)
                        | Inst::ConstBool(_)
                        | Inst::Param { .. }
                        | Inst::Add(_, _)
                        | Inst::Sub(_, _)
                        | Inst::Mul(_, _)
                        | Inst::Div(_, _)
                        | Inst::Rem(_, _)
                        | Inst::Neg(_)
                        | Inst::And(_, _)
                        | Inst::Or(_, _)
                        | Inst::Xor(_, _)
                        | Inst::Not(_)
                        | Inst::Shl(_, _)
                        | Inst::Shr(_, _)
                        | Inst::Sar(_, _)
                        | Inst::Cmp { .. }
                        | Inst::IToF(_)
                        | Inst::FToI(_)
                        | Inst::Phi { .. }
                )
            )
        });
        let terminator_supported = match block.term.as_ref() {
            Some(Terminator::Return(result)) => {
                function.types.get(result.0 as usize) == Some(&Ty::F64)
            }
            Some(Terminator::Jump(_)) => true,
            Some(Terminator::Branch { cond, .. }) => {
                matches!(function.insts.get(cond.0 as usize), Some(Inst::Cmp { .. }))
                    && function.types.get(cond.0 as usize) == Some(&Ty::Bool)
            }
            None => false,
        };
        instructions_supported && terminator_supported
    });
    if mixed_stack_spill_f64 && non_param_count > 24 {
        return emit_mixed_f64_with_stack_spills(function);
    }

    let mut registers = HashMap::<Value, Gpr>::new();
    let mut next_float_temporary = 16u8;
    let mut next_integer_temporary = 8u8;
    let mut next_saved_float = 8u8;
    let mut next_saved_integer = 19u8;
    let mut saved_float = Vec::<Gpr>::new();
    let mut saved_integer = Vec::<Gpr>::new();
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let register = match inst {
                Inst::Param { index, ty } => {
                    let Some((_, declared_ty)) = function.params.get(*index as usize) else {
                        return Err(format!("parameter index {index} is out of range"));
                    };
                    if declared_ty != ty {
                        return Err(format!("parameter {index} has inconsistent IR type"));
                    }
                    let ordinal = function.params[..*index as usize]
                        .iter()
                        .filter(|(_, candidate)| candidate == ty)
                        .count();
                    if ordinal >= 8 {
                        return Err(format!(
                            "AArch64 emitter supports at most 8 {ty:?} parameters"
                        ));
                    }
                    Gpr::new(ordinal as u8)
                }
                _ => match function.types.get(value.0 as usize) {
                    Some(Ty::F64) => {
                        let index = if next_float_temporary <= 31 {
                            let index = next_float_temporary;
                            next_float_temporary += 1;
                            index
                        } else if next_saved_float <= 15 {
                            let index = next_saved_float;
                            next_saved_float += 1;
                            saved_float.push(Gpr::new_d(index));
                            index
                        } else {
                            return Err(
                                "AArch64 emitter ran out of D-register temporaries".to_string()
                            );
                        };
                        Gpr::new_d(index)
                    }
                    Some(Ty::I64 | Ty::Bool) => {
                        let index = if next_integer_temporary <= 18 {
                            let index = next_integer_temporary;
                            next_integer_temporary += 1;
                            index
                        } else if next_saved_integer <= 28 {
                            let index = next_saved_integer;
                            next_saved_integer += 1;
                            saved_integer.push(Gpr::new(index));
                            index
                        } else {
                            return Err(
                                "AArch64 emitter ran out of X-register temporaries".to_string()
                            );
                        };
                        Gpr::new(index)
                    }
                    None => return Err(format!("value {value:?} has no AArch64 IR type")),
                },
            };
            registers.insert(value, register);
        }
    }

    let register_of = |value: Value| {
        registers
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing AArch64 register for value {value:?}"))
    };
    let mut asm = Assembler::new();
    let mut pool = Vec::<u64>::new();
    let mut pool_indices = HashMap::<u64, usize>::new();
    let mut literal_loads = Vec::<(usize, usize, Gpr)>::new();

    let mut saved_float_slots = Vec::<(Gpr, u16)>::new();
    let mut saved_integer_slots = Vec::<(Gpr, u16)>::new();
    let mut next_slot = 0u16;
    for &register in &saved_float {
        saved_float_slots.push((register, next_slot));
        next_slot += 8;
    }
    for &register in &saved_integer {
        saved_integer_slots.push((register, next_slot));
        next_slot += 8;
    }
    let frame_bytes = u16::try_from((usize::from(next_slot) + 15) & !15)
        .map_err(|_| "AArch64 stack frame is too large".to_string())?;

    if frame_bytes != 0 {
        asm.sub_imm(SP, SP, frame_bytes, false);
        for &(register, offset) in &saved_float_slots {
            asm.str_d(register, SP, offset);
        }
        for &(register, offset) in &saved_integer_slots {
            asm.str(register, SP, offset);
        }
    }

    let mut block_offsets = vec![None; function.blocks.len()];
    let mut branch_fixups = Vec::<(usize, usize, Option<Condition>)>::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        block_offsets[block_index] = Some(asm.words.len() * 4);
        for &value in &block.insts {
            let dst = registers[&value];
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            match inst {
                Inst::ConstF64(bits) => {
                    let pool_index = match pool_indices.get(bits) {
                        Some(index) => *index,
                        None => {
                            let index = pool.len();
                            pool.push(*bits);
                            pool_indices.insert(*bits, index);
                            index
                        }
                    };
                    let instruction_index = asm.words.len();
                    asm.ldr_literal_d(dst, 0);
                    literal_loads.push((instruction_index, pool_index, dst));
                }
                Inst::Param { .. } | Inst::Phi { .. } => {}
                Inst::ConstI64(number) => emit_i64_constant(&mut asm, dst, *number as u64),
                Inst::ConstBool(value) => emit_i64_constant(&mut asm, dst, u64::from(*value)),
                Inst::Add(lhs, rhs) => {
                    if function.types[value.0 as usize] == Ty::F64 {
                        asm.fadd_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                    } else {
                        asm.add_reg(dst, register_of(*lhs)?, register_of(*rhs)?);
                    }
                }
                Inst::Sub(lhs, rhs) => {
                    if function.types[value.0 as usize] == Ty::F64 {
                        asm.fsub_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                    } else {
                        asm.sub_reg(dst, register_of(*lhs)?, register_of(*rhs)?);
                    }
                }
                Inst::Mul(lhs, rhs) => {
                    if function.types[value.0 as usize] == Ty::F64 {
                        asm.fmul_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                    } else {
                        asm.mul(dst, register_of(*lhs)?, register_of(*rhs)?);
                    }
                }
                Inst::Div(lhs, rhs) => {
                    if function.types[value.0 as usize] == Ty::F64 {
                        asm.fdiv_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                    } else {
                        asm.sdiv(dst, register_of(*lhs)?, register_of(*rhs)?);
                    }
                }
                Inst::Rem(lhs, rhs) => {
                    if function.types[value.0 as usize] != Ty::I64 {
                        return Err("AArch64 remainder requires i64 operands".to_string());
                    }
                    let lhs_reg = register_of(*lhs)?;
                    let rhs_reg = register_of(*rhs)?;
                    asm.sdiv(dst, lhs_reg, rhs_reg);
                    asm.msub(dst, dst, rhs_reg, lhs_reg);
                }
                Inst::Neg(operand) => {
                    if function.types[value.0 as usize] == Ty::F64 {
                        asm.fneg_d(dst, register_of(*operand)?);
                    } else {
                        asm.sub_reg(dst, XZR, register_of(*operand)?);
                    }
                }
                Inst::Abs(value) => {
                    if function.types[value.0 as usize] != Ty::F64 {
                        return Err("AArch64 abs requires an f64 operand".to_string());
                    }
                    asm.fabs_d(dst, register_of(*value)?);
                }
                Inst::Sqrt(value) => {
                    if function.types[value.0 as usize] != Ty::F64 {
                        return Err("AArch64 sqrt requires an f64 operand".to_string());
                    }
                    asm.fsqrt_d(dst, register_of(*value)?);
                }
                Inst::Fma { a, b, c } => {
                    if function.types[value.0 as usize] != Ty::F64 {
                        return Err("AArch64 fma requires f64 operands".to_string());
                    }
                    asm.fmadd_d(dst, register_of(*a)?, register_of(*b)?, register_of(*c)?)
                }
                Inst::Min(lhs, rhs) => {
                    if function.types[value.0 as usize] != Ty::F64 {
                        return Err("AArch64 min requires f64 operands".to_string());
                    }
                    asm.fmin_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                }
                Inst::Max(lhs, rhs) => {
                    if function.types[value.0 as usize] != Ty::F64 {
                        return Err("AArch64 max requires f64 operands".to_string());
                    }
                    asm.fmax_d(dst, register_of(*lhs)?, register_of(*rhs)?);
                }
                Inst::And(lhs, rhs) => asm.and_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Or(lhs, rhs) => asm.orr_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Xor(lhs, rhs) => asm.eor_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Not(operand) => {
                    emit_i64_constant(&mut asm, dst, u64::MAX);
                    asm.eor_reg(dst, dst, register_of(*operand)?);
                }
                Inst::Shl(lhs, rhs) => asm.lsl(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Shr(lhs, rhs) => asm.lsr(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Sar(lhs, rhs) => asm.asr(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Cmp { lhs, rhs, .. } => match function.types.get(lhs.0 as usize) {
                    Some(Ty::F64) => asm.fcmp_d(register_of(*lhs)?, register_of(*rhs)?),
                    Some(Ty::I64) | Some(Ty::Bool) => {
                        asm.cmp_reg(register_of(*lhs)?, register_of(*rhs)?)
                    }
                    _ => return Err("AArch64 comparison has an invalid operand".to_string()),
                },
                Inst::IToF(value) => asm.scvtf(dst, register_of(*value)?),
                Inst::FToI(value) => asm.fcvtzs(dst, register_of(*value)?),
                Inst::Floor(..)
                | Inst::Ceil(..)
                | Inst::Round(..)
                | Inst::Trunc(..)
                | Inst::Call { .. } => {
                    return Err(format!("AArch64 f64 emitter does not support {:?}", inst))
                }
            }
        }

        match block.term.as_ref() {
            Some(Terminator::Return(result)) => {
                let Some(result_ty) = function.types.get(result.0 as usize) else {
                    return Err(format!("return value {result:?} has no type"));
                };
                if *result_ty != Ty::F64 {
                    return Err("AArch64 mixed scalar emitter requires an f64 result".to_string());
                }
                let result_register = register_of(*result)?;
                if result_register != Gpr::new(0) {
                    asm.fmov_d(Gpr::new(0), result_register);
                }
                if frame_bytes != 0 {
                    for &(register, offset) in saved_integer_slots.iter().rev() {
                        asm.ldr(register, SP, offset);
                    }
                    for &(register, offset) in saved_float_slots.iter().rev() {
                        asm.ldr_d(register, SP, offset);
                    }
                    asm.add_imm(SP, SP, frame_bytes, false);
                }
                asm.ret();
            }
            Some(Terminator::Jump(target)) => {
                let target = target.0 as usize;
                validate_target(function, target)?;
                emit_phi_edge_copies(function, target, block_index, &registers, &mut asm)?;
                let instruction_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((instruction_index, target, None));
            }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                let condition = condition_for_cmp(function, *cond)?;
                let then_target = then_.0 as usize;
                let else_target = else_.0 as usize;
                validate_target(function, then_target)?;
                validate_target(function, else_target)?;
                emit_phi_edge_copies(function, then_target, block_index, &registers, &mut asm)?;
                let conditional_index = asm.words.len();
                asm.b_cond(condition, 0);
                branch_fixups.push((conditional_index, then_target, Some(condition)));
                emit_phi_edge_copies(function, else_target, block_index, &registers, &mut asm)?;
                let else_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((else_index, else_target, None));
            }
            None => return Err(format!("AArch64 block {block_index} has no terminator")),
        }
    }

    if !pool.is_empty() && !asm.words.len().is_multiple_of(2) {
        asm.words.push(0);
    }
    let pool_start = asm.words.len() * 4;
    for (instruction_index, pool_index, dst) in literal_loads {
        let literal_offset = pool_index
            .checked_mul(8)
            .and_then(|offset| i32::try_from(offset).ok())
            .ok_or_else(|| "AArch64 literal pool is too large".to_string())?;
        let offset = pool_start as i32 + literal_offset - (instruction_index * 4) as i32;
        asm.words[instruction_index] = encode_ldr_literal_d(dst, offset);
    }
    for (instruction_index, target, condition) in branch_fixups {
        let target_offset = block_offsets[target].expect("validated block offset") as i32;
        let source_offset = (instruction_index * 4) as i32;
        let offset = target_offset - source_offset;
        asm.words[instruction_index] = match condition {
            Some(condition) => encode_branch_cond(condition, offset),
            None => encode_branch(offset),
        };
    }
    for bits in pool {
        asm.words.push(bits as u32);
        asm.words.push((bits >> 32) as u32);
    }
    Ok(asm.bytes())
}

#[derive(Clone, Copy)]
enum F64Location {
    Register(Gpr),
    Stack(u16),
}

#[derive(Clone, Copy)]
enum MixedLocation {
    Register(Gpr),
    Stack(u16),
}

/// Emits the all-f64 subset with every non-parameter value in a stack slot.
/// D29..D31 are caller-saved AAPCS64 scratch registers, so no additional
/// register save area is needed. This path also handles structured CFGs:
/// f64 phi values are written on each incoming edge, and direct f64 compare
/// conditions are re-evaluated immediately before their branch. Mixed-value
/// spilling, calls/libm, and a general live-range allocator remain separate
/// work.
fn emit_f64_with_stack_spills(function: &Function) -> Result<Vec<u8>, String> {
    if function.blocks.is_empty() {
        return Err("AArch64 emitter requires at least one block".to_string());
    }
    let contains_call = function
        .insts
        .iter()
        .any(|inst| matches!(inst, Inst::Call { .. }));

    let mut locations = HashMap::<Value, F64Location>::new();
    let mut next_slot: u16 = if contains_call { 8 } else { 0 };
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let location = match inst {
                Inst::Param { index, ty: Ty::F64 } => {
                    let index = *index as usize;
                    if index >= function.params.len() {
                        return Err(format!("parameter index {index} is out of range"));
                    }
                    let ordinal = function.params[..index]
                        .iter()
                        .filter(|(_, ty)| *ty == Ty::F64)
                        .count();
                    if ordinal >= 8 {
                        return Err("AArch64 f64 emitter supports at most 8 parameters".to_string());
                    }
                    if contains_call {
                        let slot = next_slot;
                        next_slot = next_slot
                            .checked_add(8)
                            .ok_or_else(|| "AArch64 f64 spill frame is too large".to_string())?;
                        F64Location::Stack(slot)
                    } else {
                        F64Location::Register(Gpr::new_d(ordinal as u8))
                    }
                }
                Inst::Param { .. } => {
                    return Err("AArch64 f64 spill emitter requires f64 parameters".to_string())
                }
                Inst::Cmp { .. } => {
                    if function.types.get(value.0 as usize) != Some(&Ty::Bool) {
                        return Err(format!("comparison {value:?} does not produce bool"));
                    }
                    // Branches re-evaluate direct comparisons because their
                    // result is represented by AArch64 condition flags.
                    continue;
                }
                _ => {
                    if function.types.get(value.0 as usize) != Some(&Ty::F64) {
                        return Err(format!(
                            "AArch64 f64 spill emitter does not support non-f64 value {value:?}"
                        ));
                    }
                    let slot = next_slot;
                    next_slot = next_slot
                        .checked_add(8)
                        .ok_or_else(|| "AArch64 f64 spill frame is too large".to_string())?;
                    F64Location::Stack(slot)
                }
            };
            locations.insert(value, location);
        }
    }

    let frame_bytes = u16::try_from((usize::from(next_slot) + 15) & !15)
        .map_err(|_| "AArch64 f64 spill frame is too large".to_string())?;
    if frame_bytes >= 4096 {
        return Err("AArch64 f64 spill frame exceeds immediate offset range".to_string());
    }
    let location_of = |value: Value| {
        locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing AArch64 location for value {value:?}"))
    };
    let mut asm = Assembler::new();
    let scratch_a = Gpr::new_d(29);
    let scratch_b = Gpr::new_d(30);
    let scratch_c = Gpr::new_d(31);
    asm.sub_imm(SP, SP, frame_bytes, false);
    if contains_call {
        asm.str(Gpr::new(30), SP, 0);
        for block in &function.blocks {
            for &value in &block.insts {
                let &Inst::Param { index, ty: Ty::F64 } = &function.insts[value.0 as usize] else {
                    continue;
                };
                let ordinal = function.params[..index as usize]
                    .iter()
                    .filter(|(_, ty)| *ty == Ty::F64)
                    .count();
                let F64Location::Stack(offset) = location_of(value)? else {
                    return Err(format!("parameter {value:?} was not assigned a stack slot"));
                };
                asm.str_d(Gpr::new_d(ordinal as u8), SP, offset);
            }
        }
    }

    let load = |asm: &mut Assembler, value: Value, scratch: Gpr| -> Result<(), String> {
        match location_of(value)? {
            F64Location::Register(register) => {
                if register != scratch {
                    asm.fmov_d(scratch, register);
                }
            }
            F64Location::Stack(offset) => asm.ldr_d(scratch, SP, offset),
        }
        Ok(())
    };
    let store = |asm: &mut Assembler, value: Value, source: Gpr| -> Result<(), String> {
        match location_of(value)? {
            F64Location::Register(register) => {
                if register != source {
                    asm.fmov_d(register, source);
                }
            }
            F64Location::Stack(offset) => asm.str_d(source, SP, offset),
        }
        Ok(())
    };

    let mut pool = Vec::<u64>::new();
    let mut pool_indices = HashMap::<u64, usize>::new();
    let mut literal_loads = Vec::<(usize, usize, Gpr)>::new();
    let mut block_offsets = vec![None; function.blocks.len()];
    let mut branch_fixups = Vec::<(usize, usize, Option<Condition>)>::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        block_offsets[block_index] = Some(asm.words.len() * 4);
        for &value in &block.insts {
            match function.insts.get(value.0 as usize) {
                Some(Inst::ConstF64(bits)) => {
                    let pool_index = match pool_indices.get(bits) {
                        Some(index) => *index,
                        None => {
                            let index = pool.len();
                            pool.push(*bits);
                            pool_indices.insert(*bits, index);
                            index
                        }
                    };
                    let instruction_index = asm.words.len();
                    asm.ldr_literal_d(scratch_a, 0);
                    literal_loads.push((instruction_index, pool_index, scratch_a));
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Param { .. } | Inst::Cmp { .. } | Inst::Phi { .. }) => {}
                Some(Inst::Add(lhs, rhs))
                | Some(Inst::Sub(lhs, rhs))
                | Some(Inst::Mul(lhs, rhs))
                | Some(Inst::Div(lhs, rhs))
                | Some(Inst::Min(lhs, rhs))
                | Some(Inst::Max(lhs, rhs)) => {
                    load(&mut asm, *lhs, scratch_a)?;
                    load(&mut asm, *rhs, scratch_b)?;
                    match function.insts.get(value.0 as usize) {
                        Some(Inst::Add(..)) => asm.fadd_d(scratch_a, scratch_a, scratch_b),
                        Some(Inst::Sub(..)) => asm.fsub_d(scratch_a, scratch_a, scratch_b),
                        Some(Inst::Mul(..)) => asm.fmul_d(scratch_a, scratch_a, scratch_b),
                        Some(Inst::Div(..)) => asm.fdiv_d(scratch_a, scratch_a, scratch_b),
                        Some(Inst::Min(..)) => asm.fmin_d(scratch_a, scratch_a, scratch_b),
                        Some(Inst::Max(..)) => asm.fmax_d(scratch_a, scratch_a, scratch_b),
                        _ => unreachable!(),
                    }
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Neg(operand)) => {
                    load(&mut asm, *operand, scratch_a)?;
                    asm.fneg_d(scratch_a, scratch_a);
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Abs(operand)) => {
                    load(&mut asm, *operand, scratch_a)?;
                    asm.fabs_d(scratch_a, scratch_a);
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Sqrt(operand)) => {
                    load(&mut asm, *operand, scratch_a)?;
                    asm.fsqrt_d(scratch_a, scratch_a);
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Fma { a, b, c }) => {
                    load(&mut asm, *a, scratch_a)?;
                    load(&mut asm, *b, scratch_b)?;
                    load(&mut asm, *c, scratch_c)?;
                    asm.fmadd_d(scratch_a, scratch_a, scratch_b, scratch_c);
                    store(&mut asm, value, scratch_a)?;
                }
                Some(Inst::Call { func, args }) => {
                    let expected = match func {
                        forge_ir::LibFunc::Pow => 2,
                        forge_ir::LibFunc::Sin
                        | forge_ir::LibFunc::Cos
                        | forge_ir::LibFunc::Tan
                        | forge_ir::LibFunc::Exp
                        | forge_ir::LibFunc::Log => 1,
                    };
                    if args.len() != expected
                        || args
                            .iter()
                            .any(|arg| function.types.get(arg.0 as usize) != Some(&Ty::F64))
                        || function.types.get(value.0 as usize) != Some(&Ty::F64)
                    {
                        return Err(
                            "AArch64 libm calls require f64 arguments and result".to_string()
                        );
                    }
                    load(&mut asm, args[0], scratch_a)?;
                    asm.fmov_d(Gpr::new_d(0), scratch_a);
                    if expected == 2 {
                        load(&mut asm, args[1], scratch_b)?;
                        asm.fmov_d(Gpr::new_d(1), scratch_b);
                    }
                    emit_i64_constant(&mut asm, Gpr::new(16), libm_address(*func) as u64);
                    asm.blr(Gpr::new(16));
                    store(&mut asm, value, Gpr::new_d(0))?;
                }
                Some(inst) => {
                    return Err(format!(
                        "AArch64 f64 spill emitter does not support {inst:?}"
                    ))
                }
                None => return Err(format!("missing instruction for value {value:?}")),
            }
        }

        match block.term.as_ref() {
            Some(Terminator::Return(result)) => {
                let Some(result_ty) = function.types.get(result.0 as usize) else {
                    return Err(format!("return value {result:?} has no type"));
                };
                if *result_ty != Ty::F64 {
                    return Err("AArch64 f64 spill emitter requires an f64 result".to_string());
                }
                match location_of(*result)? {
                    F64Location::Register(register) if register != Gpr::new_d(0) => {
                        asm.fmov_d(Gpr::new_d(0), register)
                    }
                    F64Location::Stack(offset) => asm.ldr_d(Gpr::new_d(0), SP, offset),
                    F64Location::Register(_) => {}
                }
                if contains_call {
                    asm.ldr(Gpr::new(30), SP, 0);
                }
                asm.add_imm(SP, SP, frame_bytes, false);
                asm.ret();
            }
            Some(Terminator::Jump(target)) => {
                let target = target.0 as usize;
                validate_target(function, target)?;
                emit_stack_phi_edge_copies(
                    function,
                    target,
                    block_index,
                    &locations,
                    &mut asm,
                    scratch_a,
                )?;
                let instruction_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((instruction_index, target, None));
            }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                let condition = emit_stack_branch_condition(
                    function, *cond, &locations, &mut asm, scratch_a, scratch_b,
                )?;
                let then_target = then_.0 as usize;
                let else_target = else_.0 as usize;
                validate_target(function, then_target)?;
                validate_target(function, else_target)?;
                emit_stack_phi_edge_copies(
                    function,
                    then_target,
                    block_index,
                    &locations,
                    &mut asm,
                    scratch_a,
                )?;
                let conditional_index = asm.words.len();
                asm.b_cond(condition, 0);
                branch_fixups.push((conditional_index, then_target, Some(condition)));
                emit_stack_phi_edge_copies(
                    function,
                    else_target,
                    block_index,
                    &locations,
                    &mut asm,
                    scratch_a,
                )?;
                let else_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((else_index, else_target, None));
            }
            None => return Err(format!("AArch64 block {block_index} has no terminator")),
        }
    }

    if !pool.is_empty() && !asm.words.len().is_multiple_of(2) {
        asm.words.push(0);
    }
    let pool_start = asm.words.len() * 4;
    for (instruction_index, pool_index, dst) in literal_loads {
        let literal_offset = pool_index
            .checked_mul(8)
            .and_then(|offset| i32::try_from(offset).ok())
            .ok_or_else(|| "AArch64 literal pool is too large".to_string())?;
        let offset = pool_start as i32 + literal_offset - (instruction_index * 4) as i32;
        asm.words[instruction_index] = encode_ldr_literal_d(dst, offset);
    }
    for bits in pool {
        asm.words.push(bits as u32);
        asm.words.push((bits >> 32) as u32);
    }
    for (instruction_index, target, condition) in branch_fixups {
        let target_offset = block_offsets[target].expect("validated block offset") as i32;
        let source_offset = (instruction_index * 4) as i32;
        let offset = target_offset - source_offset;
        asm.words[instruction_index] = match condition {
            Some(condition) => encode_branch_cond(condition, offset),
            None => encode_branch(offset),
        };
    }
    Ok(asm.bytes())
}

/// Emits a high-pressure mixed scalar function with all non-parameter values
/// in typed stack slots. X28..X30 are preserved because they are used as
/// integer scratch registers; D29..D31 remain caller-saved AAPCS64 scratch
/// registers. Structured CFGs are supported by storing typed phi values on
/// incoming edges and re-evaluating direct comparisons before branches.
/// Calls/libm and general live-range allocation remain separate work.
fn emit_mixed_f64_with_stack_spills(function: &Function) -> Result<Vec<u8>, String> {
    let mut locations = HashMap::<Value, MixedLocation>::new();
    let mut next_slot = 24u16;
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let location = match inst {
                Inst::Param { index, ty } => {
                    let index = *index as usize;
                    let Some((_, declared_ty)) = function.params.get(index) else {
                        return Err(format!("parameter index {index} is out of range"));
                    };
                    if declared_ty != ty || function.types.get(value.0 as usize) != Some(ty) {
                        return Err(format!("parameter {index} has inconsistent IR type"));
                    }
                    let ordinal = function.params[..index]
                        .iter()
                        .filter(|(_, candidate)| candidate == ty)
                        .count();
                    if ordinal >= 8 {
                        return Err(format!(
                            "AArch64 mixed spill emitter supports at most 8 {ty:?} parameters"
                        ));
                    }
                    MixedLocation::Register(match ty {
                        Ty::F64 => Gpr::new_d(ordinal as u8),
                        Ty::I64 | Ty::Bool => Gpr::new(ordinal as u8),
                    })
                }
                Inst::Cmp { .. } => {
                    if function.types.get(value.0 as usize) != Some(&Ty::Bool) {
                        return Err(format!("comparison {value:?} does not produce bool"));
                    }
                    // A comparison is represented by condition flags at a
                    // branch and therefore has no materialized stack value.
                    continue;
                }
                Inst::Call { .. } => {
                    return Err(
                        "AArch64 mixed spill emitter does not support calls or libm".to_string()
                    )
                }
                _ => {
                    if function.types.get(value.0 as usize).is_none() {
                        return Err(format!("value {value:?} has no AArch64 IR type"));
                    }
                    let slot = next_slot;
                    next_slot = next_slot
                        .checked_add(8)
                        .ok_or_else(|| "AArch64 mixed spill frame is too large".to_string())?;
                    MixedLocation::Stack(slot)
                }
            };
            locations.insert(value, location);
        }
    }

    let frame_bytes = u16::try_from((usize::from(next_slot) + 15) & !15)
        .map_err(|_| "AArch64 mixed spill frame is too large".to_string())?;
    if frame_bytes >= 4096 {
        return Err("AArch64 mixed spill frame exceeds immediate offset range".to_string());
    }
    let location_of = |value: Value| {
        locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing AArch64 location for value {value:?}"))
    };
    let mut asm = Assembler::new();
    let int_a = Gpr::new(28);
    let int_b = Gpr::new(29);
    let int_c = Gpr::new(30);
    let float_a = Gpr::new_d(29);
    let float_b = Gpr::new_d(30);
    asm.sub_imm(SP, SP, frame_bytes, false);
    for (register, offset) in [(int_a, 0), (int_b, 8), (int_c, 16)] {
        asm.str(register, SP, offset);
    }

    let load_f64 = |asm: &mut Assembler, value: Value, scratch: Gpr| -> Result<(), String> {
        if function.types.get(value.0 as usize) != Some(&Ty::F64) {
            return Err(format!("value {value:?} is not f64"));
        }
        match location_of(value)? {
            MixedLocation::Register(register) => {
                if register != scratch {
                    asm.fmov_d(scratch, register);
                }
            }
            MixedLocation::Stack(offset) => asm.ldr_d(scratch, SP, offset),
        }
        Ok(())
    };
    let load_int = |asm: &mut Assembler, value: Value, scratch: Gpr| -> Result<(), String> {
        if !matches!(
            function.types.get(value.0 as usize),
            Some(Ty::I64 | Ty::Bool)
        ) {
            return Err(format!("value {value:?} is not an integer or bool"));
        }
        match location_of(value)? {
            MixedLocation::Register(register) => {
                if register != scratch {
                    asm.orr_reg(scratch, XZR, register);
                }
            }
            MixedLocation::Stack(offset) => asm.ldr(scratch, SP, offset),
        }
        Ok(())
    };
    let store_f64 = |asm: &mut Assembler, value: Value, source: Gpr| -> Result<(), String> {
        match location_of(value)? {
            MixedLocation::Stack(offset) => asm.str_d(source, SP, offset),
            MixedLocation::Register(_) => {
                return Err(format!("non-parameter f64 value {value:?} uses a register"))
            }
        }
        Ok(())
    };
    let store_int = |asm: &mut Assembler, value: Value, source: Gpr| -> Result<(), String> {
        match location_of(value)? {
            MixedLocation::Stack(offset) => asm.str(source, SP, offset),
            MixedLocation::Register(_) => {
                return Err(format!(
                    "non-parameter integer value {value:?} uses a register"
                ))
            }
        }
        Ok(())
    };

    let mut pool = Vec::<u64>::new();
    let mut pool_indices = HashMap::<u64, usize>::new();
    let mut literal_loads = Vec::<(usize, usize, Gpr)>::new();
    let mut block_offsets = vec![None; function.blocks.len()];
    let mut branch_fixups = Vec::<(usize, usize, Option<Condition>)>::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        block_offsets[block_index] = Some(asm.words.len() * 4);
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("missing instruction for value {value:?}"));
            };
            match inst {
                Inst::ConstF64(bits) => {
                    let pool_index = match pool_indices.get(bits) {
                        Some(index) => *index,
                        None => {
                            let index = pool.len();
                            pool.push(*bits);
                            pool_indices.insert(*bits, index);
                            index
                        }
                    };
                    let instruction_index = asm.words.len();
                    asm.ldr_literal_d(float_a, 0);
                    literal_loads.push((instruction_index, pool_index, float_a));
                    store_f64(&mut asm, value, float_a)?;
                }
                Inst::ConstI64(number) => {
                    emit_i64_constant(&mut asm, int_a, *number as u64);
                    store_int(&mut asm, value, int_a)?;
                }
                Inst::ConstBool(boolean) => {
                    emit_i64_constant(&mut asm, int_a, u64::from(*boolean));
                    store_int(&mut asm, value, int_a)?;
                }
                Inst::Param { .. } | Inst::Cmp { .. } | Inst::Phi { .. } => {}
                Inst::Add(lhs, rhs)
                | Inst::Sub(lhs, rhs)
                | Inst::Mul(lhs, rhs)
                | Inst::Div(lhs, rhs) => match function.types.get(value.0 as usize) {
                    Some(Ty::F64) => {
                        load_f64(&mut asm, *lhs, float_a)?;
                        load_f64(&mut asm, *rhs, float_b)?;
                        match inst {
                            Inst::Add(..) => asm.fadd_d(float_a, float_a, float_b),
                            Inst::Sub(..) => asm.fsub_d(float_a, float_a, float_b),
                            Inst::Mul(..) => asm.fmul_d(float_a, float_a, float_b),
                            Inst::Div(..) => asm.fdiv_d(float_a, float_a, float_b),
                            _ => unreachable!(),
                        }
                        store_f64(&mut asm, value, float_a)?;
                    }
                    Some(Ty::I64) => {
                        load_int(&mut asm, *lhs, int_a)?;
                        load_int(&mut asm, *rhs, int_b)?;
                        match inst {
                            Inst::Add(..) => asm.add_reg(int_a, int_a, int_b),
                            Inst::Sub(..) => asm.sub_reg(int_a, int_a, int_b),
                            Inst::Mul(..) => asm.mul(int_a, int_a, int_b),
                            Inst::Div(..) => asm.sdiv(int_a, int_a, int_b),
                            _ => unreachable!(),
                        }
                        store_int(&mut asm, value, int_a)?;
                    }
                    _ => {
                        return Err(format!(
                            "mixed spill arithmetic has invalid type for {value:?}"
                        ))
                    }
                },
                Inst::Rem(lhs, rhs) => {
                    if function.types.get(value.0 as usize) != Some(&Ty::I64) {
                        return Err("AArch64 mixed remainder requires an i64 result".to_string());
                    }
                    load_int(&mut asm, *lhs, int_a)?;
                    load_int(&mut asm, *rhs, int_b)?;
                    asm.sdiv(int_c, int_a, int_b);
                    asm.msub(int_a, int_c, int_b, int_a);
                    store_int(&mut asm, value, int_a)?;
                }
                Inst::Neg(operand) => match function.types.get(value.0 as usize) {
                    Some(Ty::F64) => {
                        load_f64(&mut asm, *operand, float_a)?;
                        asm.fneg_d(float_a, float_a);
                        store_f64(&mut asm, value, float_a)?;
                    }
                    Some(Ty::I64) => {
                        load_int(&mut asm, *operand, int_a)?;
                        asm.sub_reg(int_a, XZR, int_a);
                        store_int(&mut asm, value, int_a)?;
                    }
                    _ => {
                        return Err(format!(
                            "mixed spill negation has invalid type for {value:?}"
                        ))
                    }
                },
                Inst::And(lhs, rhs)
                | Inst::Or(lhs, rhs)
                | Inst::Xor(lhs, rhs)
                | Inst::Shl(lhs, rhs)
                | Inst::Shr(lhs, rhs)
                | Inst::Sar(lhs, rhs) => {
                    if !matches!(
                        function.types.get(value.0 as usize),
                        Some(Ty::I64 | Ty::Bool)
                    ) {
                        return Err(format!(
                            "mixed spill logical op has invalid type for {value:?}"
                        ));
                    }
                    load_int(&mut asm, *lhs, int_a)?;
                    load_int(&mut asm, *rhs, int_b)?;
                    match inst {
                        Inst::And(..) => asm.and_reg(int_a, int_a, int_b),
                        Inst::Or(..) => asm.orr_reg(int_a, int_a, int_b),
                        Inst::Xor(..) => asm.eor_reg(int_a, int_a, int_b),
                        Inst::Shl(..) => asm.lsl(int_a, int_a, int_b),
                        Inst::Shr(..) => asm.lsr(int_a, int_a, int_b),
                        Inst::Sar(..) => asm.asr(int_a, int_a, int_b),
                        _ => unreachable!(),
                    }
                    store_int(&mut asm, value, int_a)?;
                }
                Inst::Not(operand) => {
                    if !matches!(
                        function.types.get(value.0 as usize),
                        Some(Ty::I64 | Ty::Bool)
                    ) {
                        return Err(format!("mixed spill not has invalid type for {value:?}"));
                    }
                    load_int(&mut asm, *operand, int_a)?;
                    emit_i64_constant(&mut asm, int_b, u64::MAX);
                    asm.eor_reg(int_a, int_b, int_a);
                    store_int(&mut asm, value, int_a)?;
                }
                Inst::IToF(operand) => {
                    load_int(&mut asm, *operand, int_a)?;
                    asm.scvtf(float_a, int_a);
                    store_f64(&mut asm, value, float_a)?;
                }
                Inst::FToI(operand) => {
                    load_f64(&mut asm, *operand, float_a)?;
                    asm.fcvtzs(int_a, float_a);
                    store_int(&mut asm, value, int_a)?;
                }
                inst => {
                    return Err(format!(
                        "AArch64 mixed spill emitter does not support {inst:?}"
                    ))
                }
            }
        }

        match block.term.as_ref() {
            Some(Terminator::Return(result)) => {
                if function.types.get(result.0 as usize) != Some(&Ty::F64) {
                    return Err("AArch64 mixed spill emitter requires an f64 result".to_string());
                }
                load_f64(&mut asm, *result, Gpr::new_d(0))?;
                for (register, offset) in [(int_c, 16), (int_b, 8), (int_a, 0)] {
                    asm.ldr(register, SP, offset);
                }
                asm.add_imm(SP, SP, frame_bytes, false);
                asm.ret();
            }
            Some(Terminator::Jump(target)) => {
                let target = target.0 as usize;
                validate_target(function, target)?;
                emit_mixed_stack_phi_edge_copies(
                    function,
                    target,
                    block_index,
                    &locations,
                    &mut asm,
                    float_a,
                    int_a,
                )?;
                let instruction_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((instruction_index, target, None));
            }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                let condition = emit_mixed_stack_branch_condition(
                    function, *cond, &locations, &mut asm, float_a, float_b, int_a, int_b,
                )?;
                let then_target = then_.0 as usize;
                let else_target = else_.0 as usize;
                validate_target(function, then_target)?;
                validate_target(function, else_target)?;
                emit_mixed_stack_phi_edge_copies(
                    function,
                    then_target,
                    block_index,
                    &locations,
                    &mut asm,
                    float_a,
                    int_a,
                )?;
                let conditional_index = asm.words.len();
                asm.b_cond(condition, 0);
                branch_fixups.push((conditional_index, then_target, Some(condition)));
                emit_mixed_stack_phi_edge_copies(
                    function,
                    else_target,
                    block_index,
                    &locations,
                    &mut asm,
                    float_a,
                    int_a,
                )?;
                let else_index = asm.words.len();
                asm.b(0);
                branch_fixups.push((else_index, else_target, None));
            }
            None => return Err(format!("AArch64 block {block_index} has no terminator")),
        }
    }

    if !pool.is_empty() && !asm.words.len().is_multiple_of(2) {
        asm.words.push(0);
    }
    let pool_start = asm.words.len() * 4;
    for (instruction_index, pool_index, dst) in literal_loads {
        let literal_offset = pool_index
            .checked_mul(8)
            .and_then(|offset| i32::try_from(offset).ok())
            .ok_or_else(|| "AArch64 literal pool is too large".to_string())?;
        let offset = pool_start as i32 + literal_offset - (instruction_index * 4) as i32;
        asm.words[instruction_index] = encode_ldr_literal_d(dst, offset);
    }
    for bits in pool {
        asm.words.push(bits as u32);
        asm.words.push((bits >> 32) as u32);
    }
    for (instruction_index, target, condition) in branch_fixups {
        let target_offset = block_offsets[target].expect("validated block offset") as i32;
        let source_offset = (instruction_index * 4) as i32;
        let offset = target_offset - source_offset;
        asm.words[instruction_index] = match condition {
            Some(condition) => encode_branch_cond(condition, offset),
            None => encode_branch(offset),
        };
    }
    Ok(asm.bytes())
}

fn emit_mixed_stack_phi_edge_copies(
    function: &Function,
    target: usize,
    predecessor: usize,
    locations: &HashMap<Value, MixedLocation>,
    asm: &mut Assembler,
    float_scratch: Gpr,
    int_scratch: Gpr,
) -> Result<(), String> {
    for &value in &function.blocks[target].insts {
        let Inst::Phi { incoming } = &function.insts[value.0 as usize] else {
            continue;
        };
        let Some(phi_ty) = function.types.get(value.0 as usize) else {
            return Err(format!("phi {value:?} has no AArch64 IR type"));
        };
        let Some((_, source)) = incoming
            .iter()
            .find(|(block, _)| block.0 as usize == predecessor)
        else {
            return Err(format!(
                "phi {value:?} has no incoming value for block {predecessor}"
            ));
        };
        let Some(source_ty) = function.types.get(source.0 as usize) else {
            return Err(format!("phi source {source:?} has no AArch64 IR type"));
        };
        if source_ty != phi_ty {
            return Err(format!(
                "phi {value:?} has mismatched source type {source_ty:?}"
            ));
        }
        let destination = locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing stack location for phi {value:?}"))?;
        let source_location = locations
            .get(source)
            .copied()
            .ok_or_else(|| format!("missing stack location for phi source {source:?}"))?;
        match phi_ty {
            Ty::F64 => {
                match source_location {
                    MixedLocation::Register(register) => {
                        if register != float_scratch {
                            asm.fmov_d(float_scratch, register);
                        }
                    }
                    MixedLocation::Stack(offset) => asm.ldr_d(float_scratch, SP, offset),
                }
                match destination {
                    MixedLocation::Stack(offset) => asm.str_d(float_scratch, SP, offset),
                    MixedLocation::Register(_) => {
                        return Err(format!("f64 phi {value:?} unexpectedly uses a register"))
                    }
                }
            }
            Ty::I64 | Ty::Bool => {
                match source_location {
                    MixedLocation::Register(register) => {
                        if register != int_scratch {
                            asm.orr_reg(int_scratch, XZR, register);
                        }
                    }
                    MixedLocation::Stack(offset) => asm.ldr(int_scratch, SP, offset),
                }
                match destination {
                    MixedLocation::Stack(offset) => asm.str(int_scratch, SP, offset),
                    MixedLocation::Register(_) => {
                        return Err(format!(
                            "integer phi {value:?} unexpectedly uses a register"
                        ))
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_mixed_stack_branch_condition(
    function: &Function,
    cond: Value,
    locations: &HashMap<Value, MixedLocation>,
    asm: &mut Assembler,
    lhs_float_scratch: Gpr,
    rhs_float_scratch: Gpr,
    lhs_int_scratch: Gpr,
    rhs_int_scratch: Gpr,
) -> Result<Condition, String> {
    let Some(Inst::Cmp { op, lhs, rhs }) = function.insts.get(cond.0 as usize) else {
        return Err("AArch64 mixed spill branches require a direct scalar comparison".to_string());
    };
    if function.types.get(cond.0 as usize) != Some(&Ty::Bool) {
        return Err("AArch64 mixed spill branch comparison must produce bool".to_string());
    }
    let Some(lhs_ty) = function.types.get(lhs.0 as usize) else {
        return Err(format!("comparison operand {lhs:?} has no AArch64 IR type"));
    };
    let Some(rhs_ty) = function.types.get(rhs.0 as usize) else {
        return Err(format!("comparison operand {rhs:?} has no AArch64 IR type"));
    };
    if lhs_ty != rhs_ty {
        return Err("AArch64 mixed spill comparisons require matching operand types".to_string());
    }
    match lhs_ty {
        Ty::F64 => {
            for (value, scratch) in [(*lhs, lhs_float_scratch), (*rhs, rhs_float_scratch)] {
                match locations
                    .get(&value)
                    .copied()
                    .ok_or_else(|| format!("missing location for comparison operand {value:?}"))?
                {
                    MixedLocation::Register(register) => {
                        if register != scratch {
                            asm.fmov_d(scratch, register);
                        }
                    }
                    MixedLocation::Stack(offset) => asm.ldr_d(scratch, SP, offset),
                }
            }
            asm.fcmp_d(lhs_float_scratch, rhs_float_scratch);
        }
        Ty::I64 | Ty::Bool => {
            for (value, scratch) in [(*lhs, lhs_int_scratch), (*rhs, rhs_int_scratch)] {
                match locations
                    .get(&value)
                    .copied()
                    .ok_or_else(|| format!("missing location for comparison operand {value:?}"))?
                {
                    MixedLocation::Register(register) => {
                        if register != scratch {
                            asm.orr_reg(scratch, XZR, register);
                        }
                    }
                    MixedLocation::Stack(offset) => asm.ldr(scratch, SP, offset),
                }
            }
            asm.cmp_reg(lhs_int_scratch, rhs_int_scratch);
        }
    }
    Ok(match op {
        CmpOp::Eq => Condition::Eq,
        CmpOp::Ne => Condition::Ne,
        CmpOp::Lt => Condition::Lt,
        CmpOp::Le => Condition::Le,
        CmpOp::Gt => Condition::Gt,
        CmpOp::Ge => Condition::Ge,
    })
}

/// Emits a complete AAPCS64 scalar i64 function for straight-line IR.
/// Parameters use X0..X7 and temporaries use X8..X30. Integer constants are
/// materialized with MOVZ/MOVK, so this path does not need a literal pool.
/// Control flow, boolean results, and mixed-type conversions remain separate
/// ABI work; rejecting them here is safer than silently using the f64 path.
pub fn emit_i64(function: &Function) -> Result<Vec<u8>, String> {
    if function.blocks.len() != 1 {
        return Err("AArch64 i64 emitter currently requires one straight-line block".to_string());
    }
    if function.params.iter().any(|(_, ty)| *ty != Ty::I64) {
        return Err("AArch64 i64 emitter requires i64 parameters only".to_string());
    }
    let Some(Terminator::Return(result)) = function.blocks[0].term.as_ref() else {
        return Err("AArch64 i64 emitter requires a return terminator".to_string());
    };
    if function.types.get(result.0 as usize) != Some(&Ty::I64) {
        return Err("AArch64 i64 emitter requires an i64 result".to_string());
    }

    let non_param_count = function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter(|value| {
            !matches!(
                function.insts.get(value.0 as usize),
                Some(Inst::Param { .. })
            )
        })
        .count();
    if non_param_count > 21 {
        return emit_i64_with_stack_spills(function, *result);
    }

    let mut registers = HashMap::<Value, Gpr>::new();
    let mut next_volatile = 8u8;
    let mut next_saved = 19u8;
    let mut saved = Vec::<Gpr>::new();
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let register = match inst {
                Inst::Param { index, ty: Ty::I64 } => {
                    if *index as usize >= function.params.len() || *index >= 8 {
                        return Err("AArch64 i64 emitter supports at most 8 parameters".to_string());
                    }
                    Gpr::new(*index as u8)
                }
                Inst::Param { .. } => {
                    return Err("AArch64 i64 emitter requires i64 parameters only".to_string())
                }
                _ => {
                    let index = if next_volatile <= 18 {
                        let index = next_volatile;
                        next_volatile += 1;
                        index
                    } else if next_saved <= 28 {
                        let index = next_saved;
                        next_saved += 1;
                        saved.push(Gpr::new(index));
                        index
                    } else {
                        return Err(
                            "AArch64 i64 emitter ran out of X-register temporaries".to_string()
                        );
                    };
                    Gpr::new(index)
                }
            };
            registers.insert(value, register);
        }
    }

    let register_of = |value: Value| {
        registers
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing AArch64 register for value {value:?}"))
    };
    let mut asm = Assembler::new();
    let mut saved_slots = Vec::<(Gpr, u16)>::new();
    for (slot, &register) in saved.iter().enumerate() {
        saved_slots.push((register, u16::try_from(slot * 8).unwrap()));
    }
    let frame_bytes = u16::try_from((saved_slots.len() * 8 + 15) & !15)
        .map_err(|_| "AArch64 i64 stack frame is too large".to_string())?;
    if frame_bytes != 0 {
        asm.sub_imm(SP, SP, frame_bytes, false);
        for &(register, offset) in &saved_slots {
            asm.str(register, SP, offset);
        }
    }
    for &value in &function.blocks[0].insts {
        let dst = registers[&value];
        match function.insts.get(value.0 as usize) {
            Some(Inst::ConstI64(number)) => emit_i64_constant(&mut asm, dst, *number as u64),
            Some(Inst::Param { .. }) => {}
            Some(Inst::Add(lhs, rhs)) => asm.add_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Sub(lhs, rhs)) => asm.sub_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Mul(lhs, rhs)) => asm.mul(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Div(lhs, rhs)) => asm.sdiv(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Rem(lhs, rhs)) => {
                let lhs_reg = register_of(*lhs)?;
                let rhs_reg = register_of(*rhs)?;
                asm.sdiv(dst, lhs_reg, rhs_reg);
                asm.msub(dst, dst, rhs_reg, lhs_reg);
            }
            Some(Inst::Neg(operand)) => asm.sub_reg(dst, XZR, register_of(*operand)?),
            Some(Inst::And(lhs, rhs)) => asm.and_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Or(lhs, rhs)) => asm.orr_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Xor(lhs, rhs)) => asm.eor_reg(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Not(operand)) => {
                emit_i64_constant(&mut asm, dst, u64::MAX);
                asm.eor_reg(dst, dst, register_of(*operand)?);
            }
            Some(Inst::Shl(lhs, rhs)) => asm.lsl(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Shr(lhs, rhs)) => asm.lsr(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(Inst::Sar(lhs, rhs)) => asm.asr(dst, register_of(*lhs)?, register_of(*rhs)?),
            Some(inst) => return Err(format!("AArch64 i64 emitter does not support {inst:?}")),
            None => return Err(format!("missing instruction for value {value:?}")),
        }
    }

    let result_register = register_of(*result)?;
    if result_register != Gpr::new(0) {
        // ORR Xd, XZR, Xm is the architectural MOV register alias.
        asm.orr_reg(Gpr::new(0), XZR, result_register);
    }
    if frame_bytes != 0 {
        for &(register, offset) in saved_slots.iter().rev() {
            asm.ldr(register, SP, offset);
        }
        asm.add_imm(SP, SP, frame_bytes, false);
    }
    asm.ret();
    Ok(asm.bytes())
}

#[derive(Clone, Copy)]
enum I64Location {
    Register(Gpr),
    Stack(u16),
}

/// Emits the straight-line i64 subset with every non-parameter value in a
/// stack slot. This deliberately simple spill path provides a correctness
/// fallback once the compact register-preserving path runs out of registers;
/// control-flow phi spilling and a general live-range allocator remain
/// separate work.
fn emit_i64_with_stack_spills(function: &Function, result: Value) -> Result<Vec<u8>, String> {
    let mut locations = HashMap::<Value, I64Location>::new();
    let mut next_slot = 24u16;
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let location = match inst {
                Inst::Param { index, ty: Ty::I64 } => {
                    if *index as usize >= function.params.len() || *index >= 8 {
                        return Err("AArch64 i64 emitter supports at most 8 parameters".to_string());
                    }
                    I64Location::Register(Gpr::new(*index as u8))
                }
                Inst::Param { .. } => {
                    return Err("AArch64 i64 emitter requires i64 parameters only".to_string())
                }
                _ => {
                    let slot = next_slot;
                    next_slot = next_slot
                        .checked_add(8)
                        .ok_or_else(|| "AArch64 i64 spill frame is too large".to_string())?;
                    I64Location::Stack(slot)
                }
            };
            locations.insert(value, location);
        }
    }

    let frame_bytes = u16::try_from((usize::from(next_slot) + 15) & !15)
        .map_err(|_| "AArch64 i64 spill frame is too large".to_string())?;
    let location_of = |value: Value| {
        locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing AArch64 location for value {value:?}"))
    };
    let mut asm = Assembler::new();
    asm.sub_imm(SP, SP, frame_bytes, false);
    for (register, offset) in [(Gpr::new(28), 0), (Gpr::new(29), 8), (Gpr::new(30), 16)] {
        asm.str(register, SP, offset);
    }

    let load = |asm: &mut Assembler, value: Value, scratch: Gpr| -> Result<(), String> {
        match location_of(value)? {
            I64Location::Register(register) => {
                if register != scratch {
                    asm.orr_reg(scratch, XZR, register);
                }
            }
            I64Location::Stack(offset) => asm.ldr(scratch, SP, offset),
        }
        Ok(())
    };
    let store = |asm: &mut Assembler, value: Value, source: Gpr| -> Result<(), String> {
        match location_of(value)? {
            I64Location::Register(register) => {
                if register != source {
                    asm.orr_reg(register, XZR, source);
                }
            }
            I64Location::Stack(offset) => asm.str(source, SP, offset),
        }
        Ok(())
    };

    for &value in &function.blocks[0].insts {
        match function.insts.get(value.0 as usize) {
            Some(Inst::ConstI64(number)) => {
                emit_i64_constant(&mut asm, Gpr::new(28), *number as u64);
                store(&mut asm, value, Gpr::new(28))?;
            }
            Some(Inst::Param { .. }) => {}
            Some(Inst::Add(lhs, rhs))
            | Some(Inst::Sub(lhs, rhs))
            | Some(Inst::Mul(lhs, rhs))
            | Some(Inst::Div(lhs, rhs))
            | Some(Inst::And(lhs, rhs))
            | Some(Inst::Or(lhs, rhs))
            | Some(Inst::Xor(lhs, rhs))
            | Some(Inst::Shl(lhs, rhs))
            | Some(Inst::Shr(lhs, rhs))
            | Some(Inst::Sar(lhs, rhs)) => {
                load(&mut asm, *lhs, Gpr::new(28))?;
                load(&mut asm, *rhs, Gpr::new(29))?;
                let output = match function.insts.get(value.0 as usize) {
                    Some(Inst::Add(..)) => add_reg(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Sub(..)) => sub_reg(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Mul(..)) => mul(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Div(..)) => sdiv(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::And(..)) => and_reg(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Or(..)) => orr_reg(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Xor(..)) => eor_reg(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Shl(..)) => lsl(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Shr(..)) => lsr(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    Some(Inst::Sar(..)) => asr(Gpr::new(28), Gpr::new(28), Gpr::new(29)),
                    _ => unreachable!(),
                };
                asm.words.push(output);
                store(&mut asm, value, Gpr::new(28))?;
            }
            Some(Inst::Rem(lhs, rhs)) => {
                load(&mut asm, *lhs, Gpr::new(28))?;
                load(&mut asm, *rhs, Gpr::new(29))?;
                asm.sdiv(Gpr::new(30), Gpr::new(28), Gpr::new(29));
                asm.msub(Gpr::new(28), Gpr::new(30), Gpr::new(29), Gpr::new(28));
                store(&mut asm, value, Gpr::new(28))?;
            }
            Some(Inst::Neg(operand)) => {
                load(&mut asm, *operand, Gpr::new(28))?;
                asm.sub_reg(Gpr::new(28), XZR, Gpr::new(28));
                store(&mut asm, value, Gpr::new(28))?;
            }
            Some(Inst::Not(operand)) => {
                load(&mut asm, *operand, Gpr::new(28))?;
                emit_i64_constant(&mut asm, Gpr::new(29), u64::MAX);
                asm.eor_reg(Gpr::new(28), Gpr::new(29), Gpr::new(28));
                store(&mut asm, value, Gpr::new(28))?;
            }
            Some(inst) => return Err(format!("AArch64 i64 emitter does not support {inst:?}")),
            None => return Err(format!("missing instruction for value {value:?}")),
        }
    }

    match location_of(result)? {
        I64Location::Register(register) if register != Gpr::new(0) => {
            asm.orr_reg(Gpr::new(0), XZR, register);
        }
        I64Location::Stack(offset) => asm.ldr(Gpr::new(0), SP, offset),
        I64Location::Register(_) => {}
    }
    for (register, offset) in [(Gpr::new(30), 16), (Gpr::new(29), 8), (Gpr::new(28), 0)] {
        asm.ldr(register, SP, offset);
    }
    asm.add_imm(SP, SP, frame_bytes, false);
    asm.ret();
    Ok(asm.bytes())
}

fn emit_i64_constant(asm: &mut Assembler, dst: Gpr, value: u64) {
    let mut emitted = false;
    for shift in [0u8, 16, 32, 48] {
        let immediate = ((value >> shift) & u64::from(u16::MAX)) as u16;
        if !emitted {
            asm.movz(dst, immediate, shift);
            emitted = true;
        } else if immediate != 0 {
            asm.movk(dst, immediate, shift);
        }
    }
}

fn validate_target(function: &Function, target: usize) -> Result<(), String> {
    if target >= function.blocks.len() {
        Err(format!(
            "AArch64 branch target block {target} is out of range"
        ))
    } else {
        Ok(())
    }
}

fn emit_stack_phi_edge_copies(
    function: &Function,
    target: usize,
    predecessor: usize,
    locations: &HashMap<Value, F64Location>,
    asm: &mut Assembler,
    scratch: Gpr,
) -> Result<(), String> {
    for &value in &function.blocks[target].insts {
        let Inst::Phi { incoming } = &function.insts[value.0 as usize] else {
            continue;
        };
        if function.types.get(value.0 as usize) != Some(&Ty::F64) {
            return Err(format!(
                "AArch64 f64 spill emitter requires f64 phi {value:?}"
            ));
        }
        let Some((_, source)) = incoming
            .iter()
            .find(|(block, _)| block.0 as usize == predecessor)
        else {
            return Err(format!(
                "phi {value:?} has no incoming value for block {predecessor}"
            ));
        };
        let destination = locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing stack location for phi {value:?}"))?;
        let source = locations
            .get(source)
            .copied()
            .ok_or_else(|| format!("missing stack location for phi source {source:?}"))?;
        match source {
            F64Location::Register(register) => {
                if register != scratch {
                    asm.fmov_d(scratch, register);
                }
            }
            F64Location::Stack(offset) => asm.ldr_d(scratch, SP, offset),
        }
        match destination {
            F64Location::Stack(offset) => asm.str_d(scratch, SP, offset),
            F64Location::Register(_) => {
                return Err(format!("f64 phi {value:?} unexpectedly uses a register"))
            }
        }
    }
    Ok(())
}

fn emit_stack_branch_condition(
    function: &Function,
    cond: Value,
    locations: &HashMap<Value, F64Location>,
    asm: &mut Assembler,
    lhs_scratch: Gpr,
    rhs_scratch: Gpr,
) -> Result<Condition, String> {
    let Some(Inst::Cmp { op, lhs, rhs }) = function.insts.get(cond.0 as usize) else {
        return Err("AArch64 f64 spill branches require a direct scalar comparison".to_string());
    };
    if function.types.get(lhs.0 as usize) != Some(&Ty::F64)
        || function.types.get(rhs.0 as usize) != Some(&Ty::F64)
    {
        return Err("AArch64 f64 spill branches require f64 comparison operands".to_string());
    }
    for (value, scratch) in [(*lhs, lhs_scratch), (*rhs, rhs_scratch)] {
        match locations
            .get(&value)
            .copied()
            .ok_or_else(|| format!("missing stack location for comparison operand {value:?}"))?
        {
            F64Location::Register(register) => {
                if register != scratch {
                    asm.fmov_d(scratch, register);
                }
            }
            F64Location::Stack(offset) => asm.ldr_d(scratch, SP, offset),
        }
    }
    asm.fcmp_d(lhs_scratch, rhs_scratch);
    Ok(match op {
        CmpOp::Eq => Condition::Eq,
        CmpOp::Ne => Condition::Ne,
        CmpOp::Lt => Condition::Lt,
        CmpOp::Le => Condition::Le,
        CmpOp::Gt => Condition::Gt,
        CmpOp::Ge => Condition::Ge,
    })
}

fn emit_phi_edge_copies(
    function: &Function,
    target: usize,
    predecessor: usize,
    registers: &HashMap<Value, Gpr>,
    asm: &mut Assembler,
) -> Result<(), String> {
    for &value in &function.blocks[target].insts {
        let Inst::Phi { incoming } = &function.insts[value.0 as usize] else {
            continue;
        };
        let phi_type = function.types[value.0 as usize];
        let Some((_, source)) = incoming
            .iter()
            .find(|(block, _)| block.0 as usize == predecessor)
        else {
            return Err(format!(
                "phi {value:?} has no incoming value for block {predecessor}"
            ));
        };
        let destination = registers[&value];
        let source = registers
            .get(source)
            .copied()
            .ok_or_else(|| format!("missing phi source register for {source:?}"))?;
        if destination != source {
            if phi_type == Ty::F64 {
                asm.fmov_d(destination, source);
            } else {
                asm.orr_reg(destination, XZR, source);
            }
        }
    }
    Ok(())
}

fn condition_for_cmp(function: &Function, value: Value) -> Result<Condition, String> {
    let Inst::Cmp { op, .. } = function
        .insts
        .get(value.0 as usize)
        .ok_or_else(|| format!("missing branch condition {value:?}"))?
    else {
        return Err("AArch64 branches require a direct scalar comparison".to_string());
    };
    Ok(match op {
        CmpOp::Eq => Condition::Eq,
        CmpOp::Ne => Condition::Ne,
        CmpOp::Lt => Condition::Lt,
        CmpOp::Le => Condition::Le,
        CmpOp::Gt => Condition::Gt,
        CmpOp::Ge => Condition::Ge,
    })
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

    #[test]
    fn encodes_integer_and_float_instruction_families() {
        assert_eq!(add_reg(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x8b02_0020);
        assert_eq!(sdiv(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x9ac2_0c20);
        assert_eq!(and_reg(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x8a02_0020);
        assert_eq!(cmp_reg(Gpr::new(1), Gpr::new(2)), 0xeb02_003f);
        assert_eq!(fadd_d(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x1e62_2820);
        assert_eq!(fmin_d(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x1e62_7820);
        assert_eq!(fmax_d(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x1e62_6820);
        assert_eq!(fcmp_d(Gpr::new(1), Gpr::new(2)), 0x1e62_2020);
        assert_eq!(fsqrt_d(Gpr::new(0), Gpr::new(1)), 0x1e61_c020);
        assert_eq!(
            fmadd_d(Gpr::new(0), Gpr::new(1), Gpr::new(2), Gpr::new(3)),
            0x1f42_0c20
        );
    }

    #[test]
    fn encodes_branches_and_scaled_memory_offsets() {
        assert_eq!(encode_branch(16), 0x1400_0004);
        assert_eq!(encode_branch_link(-4), 0x97ff_ffff);
        assert_eq!(encode_branch_cond(Condition::Ne, 8), 0x5400_0041);
        assert_eq!(ldr(Gpr::new(0), Gpr::new(1), 16), 0xf940_0820);
        assert_eq!(str_(Gpr::new(0), Gpr::new(1), 16), 0xf900_0820);
        assert_eq!(ldr_d(Gpr::new(0), Gpr::new(1), 16), 0xfd40_0820);
        assert_eq!(str_d(Gpr::new(0), Gpr::new(1), 16), 0xfd00_0820);
        assert_eq!(movz(Gpr::new(0), 0x1234, 16), 0xd2a2_4680);
    }

    #[test]
    fn recognizes_repeated_rotated_logical_immediates() {
        assert_eq!(encode_logical_imm(0xff, true), Some((1, 0, 7)));
        assert_eq!(encode_logical_imm(0x00ff_00ff, false), Some((0, 0, 0x37)));
        assert_eq!(encode_logical_imm(0, true), None);
        assert_eq!(encode_logical_imm(u64::MAX, true), None);
        assert!(encode_logical_imm(0x0123_4567_89ab_cdef, true).is_none());
    }

    #[test]
    fn encodes_scalar_literal_load_and_register_move() {
        assert_eq!(encode_ldr_literal_d(Gpr::new(3), 8), 0x5c00_0043);
        assert_eq!(fmov_d(Gpr::new(0), Gpr::new(3)), 0x1e60_4060);
    }

    #[test]
    fn emits_straight_line_f64_function_and_deduplicates_literals() {
        let function = forge_runtime::lower_source("x * 2.5 + 2.5").unwrap();
        let bytes = emit_f64(&function).unwrap();
        assert_eq!(
            bytes.len() % 8,
            0,
            "literal pool must be eight-byte aligned"
        );
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            words.len(),
            8,
            "two loads, two arithmetic ops, move, return, one literal"
        );
        assert_eq!(
            u64::from(words[6]) | (u64::from(words[7]) << 32),
            2.5f64.to_bits()
        );
    }

    #[test]
    fn emits_scalar_f64_min_and_max() {
        let min_function = forge_runtime::lower_source("min(x, 2.0)").unwrap();
        let max_function = forge_runtime::lower_source("max(x, 2.0)").unwrap();
        let min_words = emit_f64(&min_function)
            .unwrap()
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        let max_words = emit_f64(&max_function)
            .unwrap()
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(min_words
            .iter()
            .any(|word| word & 0xffe0_fc00 == 0x1e60_7800));
        assert!(max_words
            .iter()
            .any(|word| word & 0xffe0_fc00 == 0x1e60_6800));
    }

    #[test]
    fn integer_negation_uses_xzr_not_sp() {
        let mut asm = Assembler::new();
        asm.sub_reg(Gpr::new(8), XZR, Gpr::new(0));
        assert_eq!(asm.words(), &[sub_reg(Gpr::new(8), XZR, Gpr::new(0))]);
        assert_eq!(XZR.index(), 31);
    }

    #[test]
    fn scalar_emitter_keeps_floating_temporaries_in_caller_saved_d_registers() {
        let function = forge_runtime::lower_source("x + x").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(words[0], fadd_d(Gpr::new(16), Gpr::new(0), Gpr::new(0)));
        assert_eq!(words[1], fmov_d(Gpr::new(0), Gpr::new(16)));
        assert_eq!(words[2], 0xd65f_03c0);
    }

    #[test]
    fn mixed_scalar_emitter_keeps_integer_temporaries_in_caller_saved_x_registers() {
        let function = forge_runtime::lower_source("x + (n & 3)").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words
            .iter()
            .any(|word| *word == and_reg(Gpr::new(9), Gpr::new(0), Gpr::new(8))));
        assert!(words
            .iter()
            .any(|word| *word == fadd_d(Gpr::new(17), Gpr::new(0), Gpr::new(16))));
    }

    #[test]
    fn scalar_emitter_uses_a_frame_before_spilling_excess_temporaries() {
        let source = std::iter::repeat_n("x", 18).collect::<Vec<_>>().join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        assert!(bytes.chunks(4).any(
            |chunk| u32::from_le_bytes(chunk.try_into().unwrap()) == str_d(Gpr::new(8), SP, 0)
        ));

        let source = std::iter::repeat_n("x", 26).collect::<Vec<_>>().join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        assert!(bytes.chunks(4).any(
            |chunk| u32::from_le_bytes(chunk.try_into().unwrap()) == ldr_d(Gpr::new(29), SP, 0)
        ));
        assert!(bytes.chunks(4).any(
            |chunk| u32::from_le_bytes(chunk.try_into().unwrap()) == str_d(Gpr::new(29), SP, 0)
        ));
    }

    #[test]
    fn emits_cfg_f64_stack_spills_and_phi_edge_stores() {
        let then_expr = std::iter::repeat_n("x", 14).collect::<Vec<_>>().join(" + ");
        let else_expr = std::iter::repeat_n("-x", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let source = format!("if x > 0.0 then {then_expr} else {else_expr}");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == ldr_d(Gpr::new(29), SP, 8)));
        assert!(words.iter().any(|word| *word == str_d(Gpr::new(29), SP, 0)));
        assert!(words.iter().any(|word| *word & 0x7f00_0000 == 0x5400_0000));
    }

    #[test]
    fn emits_mixed_f64_stack_spills_for_both_register_banks() {
        let source = std::iter::repeat_n("x + (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == ldr(Gpr::new(28), SP, 32)));
        assert!(words.iter().any(|word| *word == str_(Gpr::new(28), SP, 32)));
        assert!(words
            .iter()
            .any(|word| *word == ldr_d(Gpr::new(30), SP, 40)));
        assert!(words
            .iter()
            .any(|word| *word == str_d(Gpr::new(29), SP, 40)));
    }

    #[test]
    fn emits_mixed_cfg_stack_spills_and_typed_phi_edge_stores() {
        let then_expr = std::iter::repeat_n("x + (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let else_expr = std::iter::repeat_n("x - (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let source = format!("if (n & 1) > 0 then {then_expr} else {else_expr}");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == ldr(Gpr::new(28), SP, 32)));
        assert!(words.iter().any(|word| *word == str_(Gpr::new(28), SP, 32)));
        assert!(words.iter().any(|word| *word & 0x7f00_0000 == 0x5400_0000));
        assert!(
            words
                .iter()
                .filter(|word| **word & 0xffc0_0000 == 0x9e40_0000)
                .count()
                >= 2
        );
    }

    #[test]
    fn emits_mixed_cfg_integer_phi_stack_spills() {
        let source = std::iter::repeat_n("x + (if (n & 1) > 0 then 1 else 2)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        let integer_load = ldr(Gpr::new(28), SP, 0) & 0xffc0_03ff;
        let integer_store = str_(Gpr::new(28), SP, 0) & 0xffc0_03ff;
        assert!(words
            .iter()
            .any(|word| *word & 0xffc0_03ff == integer_store));
        assert!(words.iter().any(|word| *word & 0xffc0_03ff == integer_load));
        assert!(words.iter().any(|word| *word & 0x7f00_0000 == 0x5400_0000));
    }

    #[test]
    fn emits_aarch64_libm_calls_with_aligned_indirect_dispatch() {
        let function = forge_runtime::lower_source("sin(x) + pow(y, 2.0)").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == blr(Gpr::new(16))));
        assert!(words.iter().any(|word| *word == str_d(Gpr::new(0), SP, 8)));
        assert!(words.iter().any(|word| *word == str_d(Gpr::new(1), SP, 16)));
        assert!(words.contains(&0xd65f_03c0));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_f64_function_with_stack_spills() {
        let source = std::iter::repeat_n("x", 26).collect::<Vec<_>>().join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 78.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_cfg_f64_function_with_stack_spills() {
        let then_expr = std::iter::repeat_n("x", 14).collect::<Vec<_>>().join(" + ");
        let else_expr = std::iter::repeat_n("-x", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let source = format!("if x > 0.0 then {then_expr} else {else_expr}");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 42.0);
        assert_eq!(compiled.call_args(&[-3.0]), 42.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_mixed_f64_function_with_stack_spills() {
        let source = std::iter::repeat_n("x + (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(f64, i64) -> f64
        // convention, and the executable buffer remains alive for both calls.
        let function: unsafe extern "C" fn(f64, i64) -> f64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        assert_eq!(unsafe { function(2.0, 3) }, 42.0);
        assert_eq!(unsafe { function(2.0, 4) }, 28.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_mixed_cfg_function_with_stack_spills() {
        let then_expr = std::iter::repeat_n("x + (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let else_expr = std::iter::repeat_n("x - (n & 1)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let source = format!("if (n & 1) > 0 then {then_expr} else {else_expr}");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(f64, i64) -> f64
        // convention, and the executable buffer remains alive for both calls.
        let function: unsafe extern "C" fn(f64, i64) -> f64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        assert_eq!(unsafe { function(2.0, 3) }, 42.0);
        assert_eq!(unsafe { function(2.0, 4) }, 28.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_mixed_cfg_integer_phi_stack_spills() {
        let source = std::iter::repeat_n("x + (if (n & 1) > 0 then 1 else 2)", 14)
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(f64, i64) -> f64
        // convention, and the executable buffer remains alive for both calls.
        let function: unsafe extern "C" fn(f64, i64) -> f64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        assert_eq!(unsafe { function(2.0, 3) }, 42.0);
        assert_eq!(unsafe { function(2.0, 4) }, 56.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_libm_calls_with_aapcs64_float_arguments() {
        let function = forge_runtime::lower_source("sin(x) + pow(y, 2.0)").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(f64, f64) -> f64
        // convention, and the executable buffer remains alive for the call.
        let function: unsafe extern "C" fn(f64, f64) -> f64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        let result = unsafe { function(0.5, 2.0) };
        assert_eq!(result.to_bits(), (0.5f64.sin() + 4.0).to_bits());
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn native_min_max_match_interpreter_nan_semantics() {
        let min = forge_runtime::lower_source("min(x, 1.0)").unwrap();
        let max = forge_runtime::lower_source("max(x, 1.0)").unwrap();
        let run = |function: &Function| {
            let bytes = emit_f64(function).unwrap();
            let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
            buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
            buffer.make_executable().unwrap();
            forge_mem::CompiledExpr::from_buffer(buffer, 1).call_args(&[f64::NAN])
        };
        assert_eq!(run(&min).to_bits(), 1.0f64.to_bits());
        assert_eq!(run(&max).to_bits(), 1.0f64.to_bits());
    }

    #[test]
    fn emits_control_flow_and_phi_edge_copies() {
        let function = forge_runtime::lower_source("if x > 0.0 then x else -x").unwrap();
        let bytes = emit_f64(&function).unwrap();
        assert!(bytes.windows(4).any(|word| {
            u32::from_le_bytes(word.try_into().unwrap()) & 0x7f00_0000 == 0x5400_0000
        }));
    }

    #[test]
    fn emits_straight_line_i64_arithmetic_without_a_literal_pool() {
        let function = forge_runtime::lower_source("n % 7 + (n >> 2) + ~n").unwrap();
        let bytes = emit_i64(&function).unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x9ac0_0000));
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x9b00_0000));
        assert_eq!(words.last(), Some(&0xd65f_03c0));
    }

    #[test]
    fn i64_emitter_preserves_callee_saved_temporaries_in_a_frame() {
        let source = (1..=6)
            .map(|mask| format!("(n & {mask})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_i64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == str_(Gpr::new(19), SP, 0)));
        assert!(words.iter().any(|word| *word == ldr(Gpr::new(19), SP, 0)));
        assert!(words
            .iter()
            .any(|word| *word == encode_add_sub_imm(true, SP, SP, 48, false)));
        assert!(words
            .iter()
            .any(|word| *word == encode_add_sub_imm(false, SP, SP, 48, false)));
        assert_eq!(words.last(), Some(&0xd65f_03c0));
    }

    #[test]
    fn i64_emitter_spills_excess_temporaries_to_stack_slots() {
        let source = (1..=10)
            .map(|mask| format!("(n & {mask})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_i64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word == str_(Gpr::new(28), SP, 24)));
        assert!(words.iter().any(|word| *word == ldr(Gpr::new(29), SP, 24)));
        assert!(words.iter().any(|word| *word == ldr(Gpr::new(28), SP, 0)));
        assert!(words.iter().any(|word| *word == str_(Gpr::new(30), SP, 16)));
        assert_eq!(words.last(), Some(&0xd65f_03c0));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_i64_function_with_a_callee_saved_temporary_frame() {
        let source = (1..=6)
            .map(|mask| format!("(n & {mask})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_i64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(i64) -> i64
        // convention, and the executable buffer remains alive for the call.
        let function: unsafe extern "C" fn(i64) -> i64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        assert_eq!(unsafe { function(3) }, 9);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_i64_function_with_stack_spills() {
        let source = (1..=10)
            .map(|mask| format!("(n & {mask})"))
            .collect::<Vec<_>>()
            .join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_i64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        // SAFETY: the emitted body follows the AAPCS64 fn(i64) -> i64
        // convention, and the executable buffer remains alive for the call.
        let function: unsafe extern "C" fn(i64) -> i64 =
            unsafe { std::mem::transmute(buffer.as_ptr()) };
        assert_eq!(unsafe { function(3) }, 15);
    }

    #[test]
    fn i64_emitter_rejects_mixed_parameters_explicitly() {
        let function = forge_runtime::lower_source("x + 1").unwrap();
        let error = emit_i64(&function).unwrap_err();
        assert!(error.contains("i64 parameters only"));
    }

    #[test]
    fn emits_mixed_i64_f64_parameters_and_conversion() {
        let function = forge_runtime::lower_source("x + (n & 3)").unwrap();
        let bytes = emit_f64(&function).unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x8a00_0000));
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x9e40_0000));
        assert_eq!(words.last(), Some(&0xd65f_03c0));
    }

    #[test]
    fn emits_mixed_integer_comparison_branch_to_f64_result() {
        let function =
            forge_runtime::lower_source("if (n & 1) > 0 then x + 1.0 else x - 1.0").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let words = bytes
            .chunks(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| *word & 0xffc0_001f == 0xeb00_001f));
        assert!(words.iter().any(|word| *word & 0x7f00_0000 == 0x5400_0000));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_distinct_literal_pool_entries_on_native_aarch64() {
        let function = forge_runtime::lower_source("if x > 0.0 then x * 2.0 else -x").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 6.0);
        assert_eq!(compiled.call_args(&[-3.0]), 3.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_emitted_f64_function_on_native_aarch64() {
        let function = forge_runtime::lower_source("x * 2.5 + 2.5").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 10.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_emitted_conditional_on_native_aarch64() {
        let function = forge_runtime::lower_source("if x > 0.0 then x else -x").unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 3.0);
        assert_eq!(compiled.call_args(&[-3.0]), 3.0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_aarch64_function_with_a_callee_saved_temporary_frame() {
        let source = std::iter::repeat_n("x", 18).collect::<Vec<_>>().join(" + ");
        let function = forge_runtime::lower_source(&source).unwrap();
        let bytes = emit_f64(&function).unwrap();
        let mut buffer = forge_mem::ExecutableBuffer::new(bytes.len()).unwrap();
        buffer.write(|slot| slot[..bytes.len()].copy_from_slice(&bytes));
        buffer.make_executable().unwrap();
        let compiled = forge_mem::CompiledExpr::from_buffer(buffer, 1);
        assert_eq!(compiled.call_args(&[3.0]), 54.0);
    }
}
