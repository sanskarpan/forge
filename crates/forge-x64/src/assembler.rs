use crate::PhysReg;

/// A group-1 arithmetic/logic/compare operation: `add`/`or`/`and`/`sub`/
/// `xor`/`cmp` share real x86-64 encoding structure (the same "ModRM.reg
/// as opcode extension" trick for immediate forms, r/r opcodes offset by
/// a fixed stride) -- this enum carries each operation's two opcode facts
/// instead of duplicating the encoding logic once per operation. `adc`(/2)
/// and `sbb`(/3) exist in the same family but aren't part of this crate's
/// instruction set yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AluOp {
    Add,
    Or,
    And,
    Sub,
    Xor,
    Cmp,
}

impl AluOp {
    /// The ModRM.reg "opcode extension" digit used by the immediate forms
    /// (0x81/0x83 /n).
    fn extension(self) -> u8 {
        match self {
            AluOp::Add => 0,
            AluOp::Or => 1,
            AluOp::And => 4,
            AluOp::Sub => 5,
            AluOp::Xor => 6,
            AluOp::Cmp => 7,
        }
    }

    /// The direct r/r opcode -- store-direction, same convention as
    /// `mov_reg_reg`'s 0x89 (ModRM.rm is the destination, ModRM.reg is
    /// the source).
    fn rr_opcode(self) -> u8 {
        match self {
            AluOp::Add => 0x01,
            AluOp::Or => 0x09,
            AluOp::And => 0x21,
            AluOp::Sub => 0x29,
            AluOp::Xor => 0x31,
            AluOp::Cmp => 0x39,
        }
    }
}

/// Emits x86-64 machine code byte by byte, tracking label positions and
/// pending forward-jump fixups.
pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

/// An opaque handle to a not-yet-necessarily-bound code position, created
/// by `Assembler::new_label` and resolved by `Assembler::bind`. Only valid
/// for the `Assembler` instance that created it -- it's an unowned index
/// into that assembler's private `labels` vector, so passing one to a
/// different `Assembler` would index the wrong vector entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(usize);

/// A pending forward reference: at the time this was recorded, `target`
/// wasn't bound yet, so 4 placeholder rel32 displacement bytes were
/// written at `at` and must be patched once `target` is bound. There is
/// no `Rel8` variant here -- per `jmp()`'s policy below, forward jumps
/// always use rel32 (backward jumps compute their distance immediately
/// and never need a fixup at all), so a rel8 fixup kind would be
/// unconstructed dead code today. Add one back if a future instruction
/// (e.g. a conditional jump) genuinely needs optimistic-rel8 forward-fixup
/// behavior.
struct Fixup {
    at: usize,
    target: Label,
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// Creates a new, not-yet-bound label.
    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Records the label's address as the current end of `code`, then
    /// resolves every pending fixup that targets it by patching the
    /// placeholder displacement bytes reserved at fixup-creation time.
    ///
    /// # Panics
    ///
    /// Panics if `label` was already bound. Rebinding would silently
    /// corrupt codegen: any jumps already resolved against the first
    /// binding stay frozen at that (now stale) address, while jumps
    /// recorded after this second call would resolve against the new one
    /// -- two inconsistent notions of "where the label is" baked into the
    /// same buffer, with no error anywhere. This is a caller bug, and in a
    /// JIT compiler a wrong jump target is a correctness issue, so it must
    /// fail loudly in release builds too, not just under `debug_assert!`.
    pub fn bind(&mut self, label: Label) {
        assert!(
            self.labels[label.0].is_none(),
            "label {:?} was already bound -- rebinding would silently corrupt any jumps already resolved against its first position",
            label
        );
        let pos = self.code.len();
        self.labels[label.0] = Some(pos);

        let mut i = 0;
        while i < self.fixups.len() {
            if self.fixups[i].target == label {
                let fixup = self.fixups.remove(i);
                self.patch_fixup(&fixup, pos);
            } else {
                i += 1;
            }
        }
    }

    fn patch_fixup(&mut self, fixup: &Fixup, target_pos: usize) {
        let rel = target_pos as isize - (fixup.at + 4) as isize;
        let bytes = (rel as i32).to_le_bytes();
        self.code[fixup.at..fixup.at + 4].copy_from_slice(&bytes);
    }

    /// `jmp label` -- unconditional jump.
    ///
    /// Backward jumps (label already bound): the exact byte distance is
    /// known immediately, so this picks rel8 if it fits, else rel32 --
    /// encoded directly, no fixup needed since nothing about a backward
    /// jump's distance can change later.
    ///
    /// Forward jumps (label not yet bound): the real distance isn't
    /// knowable until `bind()` runs later. True "promote rel8 to rel32 in
    /// place" would require shifting every byte after the insertion point
    /// and adjusting every later label position and pending fixup -- real
    /// complexity this project's design doc deliberately opts out of for a
    /// JIT compiling small expressions. So forward jumps unconditionally
    /// emit rel32 and record a fixup, resolved once `bind()` runs.
    pub fn jmp(&mut self, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end_if_rel8 = self.code.len() + 2;
            let rel = target_pos as isize - end_if_rel8 as isize;
            if let Ok(rel8) = i8::try_from(rel) {
                self.code.push(0xEB);
                self.code.push(rel8 as u8);
            } else {
                let end_if_rel32 = self.code.len() + 5;
                let rel32 = target_pos as isize - end_if_rel32 as isize;
                self.code.push(0xE9);
                self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
            }
        } else {
            self.code.push(0xE9);
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]); // placeholder, patched by bind()
            self.fixups.push(Fixup { at, target: label });
        }
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

    /// `op dst, src` -- e.g. `add rax, rbx`. Same shape as `mov_reg_reg`:
    /// ModRM.rm is the destination, ModRM.reg is the source.
    pub fn alu_reg_reg(&mut self, op: AluOp, dst: PhysReg, src: PhysReg) {
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(op.rr_opcode());
        self.modrm_reg(src.encoding(), dst.encoding());
    }

    /// `op dst, imm` -- e.g. `add rax, 5`. ModRM.reg holds the opcode
    /// extension digit (not a register), ModRM.rm holds dst. Auto-selects
    /// the compact `83 /n ib` (imm8) form when `imm` fits in i8, else the
    /// general `81 /n id` (imm32) form -- both sign-extend to 64 bits.
    pub fn alu_reg_imm(&mut self, op: AluOp, dst: PhysReg, imm: i32) {
        self.rex(true, 0, 0, dst.encoding());
        if let Ok(imm8) = i8::try_from(imm) {
            self.code.push(0x83);
            self.modrm_reg(op.extension(), dst.encoding());
            self.code.push(imm8 as u8);
        } else {
            self.code.push(0x81);
            self.modrm_reg(op.extension(), dst.encoding());
            self.code.extend_from_slice(&imm.to_le_bytes());
        }
    }

    /// `imul dst, src` -- dst *= src. REX.W + 0F AF /r. Unlike
    /// `alu_reg_reg`'s group-1 ops, this is a LOAD-direction opcode:
    /// ModRM.reg is the destination, ModRM.rm is the source. This isn't a
    /// design choice -- it's the only two-operand IMUL r64,r/m64 encoding
    /// x86-64 has. Do not copy `alu_reg_reg`'s reg/rm assignment here.
    pub fn imul_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0xAF);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `imul dst, src, imm` -- dst = src * imm (three-operand,
    /// non-destructive). REX.W + 69 /r id. Same reg=dst/rm=src direction
    /// as `imul_reg_reg` (consistent with itself, still opposite to
    /// group-1's convention).
    pub fn imul_reg_reg_imm32(&mut self, dst: PhysReg, src: PhysReg, imm: i32) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x69);
        self.modrm_reg(dst.encoding(), src.encoding());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst, value` -- auto-selects the compact sign-extended-imm32
    /// form (REX.W + C7 /0 id) when `value` fits in i32, else the full
    /// 10-byte "movabs" form (REX.W + B8+rd io). The movabs form has NO
    /// ModRM byte at all -- the destination register is encoded directly
    /// into the low 3 bits of the opcode byte, with REX.B (not REX.R)
    /// covering register extension.
    pub fn mov_reg_imm(&mut self, dst: PhysReg, value: i64) {
        if let Ok(imm32) = i32::try_from(value) {
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xC7);
            self.modrm_reg(0, dst.encoding());
            self.code.extend_from_slice(&imm32.to_le_bytes());
        } else {
            // reg=0 here is fictitious, not an extension digit: the movabs
            // form has no ModRM byte, so there's nothing for REX.R to
            // cover. dst's extension is carried entirely through REX.B via
            // the `rm` argument below and the opcode byte's low 3 bits.
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xB8 + (dst.encoding() & 7));
            self.code.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// `mov [base + disp], src` -- 64-bit store. REX.W + 89 /r: the
    /// mirror image of `mov_reg_mem` (which uses 0x8B, load direction).
    /// Reuses `modrm_mem` directly, just swapping which operand is
    /// register vs. memory relative to `mov_reg_mem`'s call shape.
    pub fn mov_mem_reg(&mut self, base: PhysReg, disp: i32, src: PhysReg) {
        self.rex(true, src.encoding(), 0, base.encoding());
        self.code.push(0x89);
        self.modrm_mem(src.encoding(), base.encoding(), disp);
    }

    /// `test a, b` -- computes `a & b`, discards the result, sets flags
    /// only. REX.W + 85 /r. Symmetric: unlike mov/alu's store-direction
    /// convention, there's no meaningful "which operand is destination"
    /// distinction to get backward, since neither operand is written.
    pub fn test_reg_reg(&mut self, a: PhysReg, b: PhysReg) {
        self.rex(true, b.encoding(), 0, a.encoding());
        self.code.push(0x85);
        self.modrm_reg(b.encoding(), a.encoding());
    }

    /// `test dst, imm` -- REX.W + F7 /0 id. A completely separate opcode
    /// from group-1's 0x81/0x83 -- no imm8 form exists for `test` in
    /// real x86-64.
    pub fn test_reg_imm(&mut self, dst: PhysReg, imm: i32) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(0, dst.encoding());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }
}

/// One of x86-64's 16 condition codes, usable with `setcc`/`cmovcc`/`jcc`.
/// forge's current i64 comparisons only need Equal/NotEqual/Less/
/// GreaterOrEqual/LessOrEqual/Greater, but all 16 are implemented now --
/// the other 10 (unsigned, sign, overflow, parity) will matter once
/// forge grows unsigned or float comparisons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConditionCode {
    Overflow,
    NotOverflow,
    Below,
    AboveOrEqual,
    Equal,
    NotEqual,
    BelowOrEqual,
    Above,
    Sign,
    NotSign,
    Parity,
    NotParity,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    Greater,
}

impl ConditionCode {
    /// The 4-bit "cc" nibble -- the low 4 bits of the corresponding
    /// Jcc/SETcc/CMOVcc opcode byte, matching Intel's canonical ordering.
    fn nibble(self) -> u8 {
        use ConditionCode::*;
        match self {
            Overflow => 0,
            NotOverflow => 1,
            Below => 2,
            AboveOrEqual => 3,
            Equal => 4,
            NotEqual => 5,
            BelowOrEqual => 6,
            Above => 7,
            Sign => 8,
            NotSign => 9,
            Parity => 10,
            NotParity => 11,
            Less => 12,
            GreaterOrEqual => 13,
            LessOrEqual => 14,
            Greater => 15,
        }
    }
}

impl Assembler {
    /// Forces a REX prefix (even a "no-op" 0x40) when `dst` is in the
    /// 4-7 encoding range, to select spl/bpl/sil/dil instead of
    /// ah/ch/dh/bh for byte-sized operations -- the third REX trap
    /// SPEC.md warns about, and the first place in this crate that
    /// writes a byte-sized destination. Encodings 0-3 are unambiguous
    /// either way (al/cl/dl/bl); encodings 8-15 already force a REX
    /// prefix via their extension bit through the normal rex() path.
    fn rex_for_byte_dst(&mut self, dst: u8) {
        if (4..=7).contains(&dst) {
            self.code.push(0x40);
        } else {
            self.rex(false, 0, 0, dst);
        }
    }

    /// `setcc dst` -- writes 0 or 1 to the low byte of `dst` based on
    /// `cc`. 0F 90+cc /0. Writes ONLY one byte -- the upper bits of
    /// `dst` are left as whatever was there before; producing a clean
    /// full-width 0/1 is instruction-selection's job (e.g. an xor
    /// before this call), not this method's.
    pub fn setcc(&mut self, cc: ConditionCode, dst: PhysReg) {
        self.rex_for_byte_dst(dst.encoding());
        self.code.push(0x0F);
        self.code.push(0x90 + cc.nibble());
        self.modrm_reg(0, dst.encoding());
    }

    /// `cmovcc dst, src` -- dst = src if cc holds, else dst unchanged.
    /// REX.W + 0F 40+cc /r. Load-direction (reg=dst, rm=src), same
    /// convention as imul_reg_reg. No byte-register concern -- x86 has
    /// no 8-bit CMOVcc.
    pub fn cmovcc(&mut self, cc: ConditionCode, dst: PhysReg, src: PhysReg) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x40 + cc.nibble());
        self.modrm_reg(dst.encoding(), src.encoding());
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
