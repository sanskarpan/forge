//! Small, real AArch64 scalar encoder used as the foundation for the full
//! backend. Instructions are kept as 32-bit words until [`Assembler::bytes`]
//! serializes them in architectural little-endian order.

use forge_ir::{CmpOp, Function, Inst, Terminator, Ty, Value};
use std::collections::HashMap;

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
    rr(0x1e60_5800, dst, lhs, rhs)
}

pub fn fmax_d(dst: Gpr, lhs: Gpr, rhs: Gpr) -> u32 {
    rr(0x1e60_4800, dst, lhs, rhs)
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

/// Emits a complete AAPCS64 scalar f64 function for the supported IR subset.
/// Parameters use D0..D7, temporaries use D8..D30, and constants are loaded
/// from an aligned literal pool placed after the code. Arithmetic and f64
/// comparisons are emitted directly; branch edges materialize SSA φ values
/// before control transfer. Integer values, conversions, and libm calls return
/// an explicit error until their AArch64 ABI and lowering rules are implemented.
pub fn emit_f64(function: &Function) -> Result<Vec<u8>, String> {
    if function.blocks.is_empty() {
        return Err("AArch64 emitter requires at least one block".to_string());
    }
    if function.params.iter().any(|(_, ty)| *ty != Ty::F64) {
        return Err("AArch64 emitter currently accepts f64 parameters and result only".to_string());
    }

    let mut registers = HashMap::<Value, Gpr>::new();
    for block in &function.blocks {
        for &value in &block.insts {
            let Some(inst) = function.insts.get(value.0 as usize) else {
                return Err(format!("block references missing instruction {value:?}"));
            };
            let register = match inst {
                Inst::Param { index, ty: Ty::F64 } => {
                    let Some((_, _)) = function.params.get(*index as usize) else {
                        return Err(format!("parameter index {index} is out of range"));
                    };
                    let ordinal = function.params[..*index as usize]
                        .iter()
                        .filter(|(_, ty)| *ty == Ty::F64)
                        .count();
                    if ordinal >= 8 {
                        return Err("AArch64 emitter supports at most 8 f64 parameters".to_string());
                    }
                    Gpr::new(ordinal as u8)
                }
                Inst::Param { .. } => {
                    return Err("AArch64 emitter currently accepts f64 parameters only".to_string())
                }
                _ => {
                    let index = u8::try_from(value.0)
                        .ok()
                        .and_then(|index| index.checked_add(8))
                        .filter(|index| *index < 31)
                        .ok_or_else(|| {
                            "AArch64 emitter ran out of D-register temporaries".to_string()
                        })?;
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
    let mut pool = Vec::<u64>::new();
    let mut pool_indices = HashMap::<u64, usize>::new();
    let mut literal_loads = Vec::<(usize, usize, Gpr)>::new();

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
                Inst::Add(lhs, rhs) => asm.fadd_d(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Sub(lhs, rhs) => asm.fsub_d(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Mul(lhs, rhs) => asm.fmul_d(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Div(lhs, rhs) => asm.fdiv_d(dst, register_of(*lhs)?, register_of(*rhs)?),
                Inst::Neg(value) => asm.fneg_d(dst, register_of(*value)?),
                Inst::Abs(value) => asm.fabs_d(dst, register_of(*value)?),
                Inst::Sqrt(value) => asm.fsqrt_d(dst, register_of(*value)?),
                Inst::Fma { a, b, c } => {
                    asm.fmadd_d(dst, register_of(*a)?, register_of(*b)?, register_of(*c)?)
                }
                Inst::Cmp { lhs, rhs, .. } => {
                    if function.types[lhs.0 as usize] != Ty::F64
                        || function.types[rhs.0 as usize] != Ty::F64
                    {
                        return Err("AArch64 f64 comparisons require f64 operands".to_string());
                    }
                    asm.fcmp_d(register_of(*lhs)?, register_of(*rhs)?);
                }
                Inst::ConstI64(_)
                | Inst::ConstBool(_)
                | Inst::Rem(..)
                | Inst::And(..)
                | Inst::Or(..)
                | Inst::Xor(..)
                | Inst::Not(..)
                | Inst::Shl(..)
                | Inst::Shr(..)
                | Inst::Sar(..)
                | Inst::Min(..)
                | Inst::Max(..)
                | Inst::Floor(..)
                | Inst::Ceil(..)
                | Inst::Round(..)
                | Inst::Trunc(..)
                | Inst::Call { .. }
                | Inst::IToF(..)
                | Inst::FToI(..) => {
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
                    return Err(
                        "AArch64 emitter currently accepts f64 parameters and result only"
                            .to_string(),
                    );
                }
                let result_register = register_of(*result)?;
                if result_register != Gpr::new(0) {
                    asm.fmov_d(Gpr::new(0), result_register);
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

    let mut registers = HashMap::<Value, Gpr>::new();
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
                    let index = u8::try_from(value.0)
                        .ok()
                        .and_then(|index| index.checked_add(8))
                        .filter(|index| *index < 31)
                        .ok_or_else(|| {
                            "AArch64 i64 emitter ran out of X-register temporaries".to_string()
                        })?;
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
            Some(Inst::Neg(operand)) => asm.sub_reg(dst, SP, register_of(*operand)?),
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
        asm.orr_reg(Gpr::new(0), SP, result_register);
    }
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
        if function.types[value.0 as usize] != Ty::F64 {
            return Err("AArch64 emitter currently supports f64 phi values only".to_string());
        }
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
            asm.fmov_d(destination, source);
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
        return Err("AArch64 branches currently require a direct f64 comparison".to_string());
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
        assert_eq!(fadd_d(Gpr::new(0), Gpr::new(1), Gpr::new(2)), 0x1e62_2820);
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
            .chunks_exact(4)
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
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x9ac0_0000));
        assert!(words.iter().any(|word| word & 0xffc0_0000 == 0x9b00_0000));
        assert_eq!(words.last(), Some(&0xd65f_03c0));
    }

    #[test]
    fn i64_emitter_rejects_mixed_parameters_explicitly() {
        let function = forge_runtime::lower_source("x + 1").unwrap();
        let error = emit_i64(&function).unwrap_err();
        assert!(error.contains("i64 parameters only"));
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
}
