# Design: forge Phase 6d — x86-64 Shifts, Unary Ops, LEA, and 128-bit Division

**Status:** Approved for planning
**Scope:** The fourth sub-slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) — `neg`/`not`/`inc`/`dec`, `shl`/`shr`/`sar` (imm8 and CL forms), `lea` (including the 3-operand scaled-index form `lea r, [base + index*scale]`), and the 128-bit `imul` form plus `idiv` (the RDX:RAX-pair instructions Phase 4's already-built magic-number division strength reduction needs to actually execute). `cqo` (sign-extend RAX into RDX:RAX) is included alongside `idiv` even though it isn't literally in CHECKLIST's bullet — `idiv` is close to unusable without it.
**Out of scope (deferred):** `push`/`pop`/`call`/`ret` — real calling-convention work, better suited to sit next to Phase 7's prologue/instruction-selection work rather than being encoder work built in isolation. The `D1 /n` shift-by-1 special encoding (a pure 1-byte code-size optimization with no correctness implication, unlike the imm8/imm32/rel8/rel32/compact/movabs auto-selections in 6a-6c, all of which trade off real size differences that matter for jump range or constant loading) — just the general `C1 /n ib` and `D3 /n` forms, per CHECKLIST's literal "imm8 and CL forms" wording.

## Architecture

All new methods live in `crates/forge-x64/src/assembler.rs`, built on 6a-6c's existing `rex()`/`modrm_reg()`/`modrm_mem()`/`DispMode` machinery. Four independent groups:

1. `not_reg`/`neg_reg`/`inc_reg`/`dec_reg` — four trivial standalone methods, deliberately NOT unified under a shared enum (unlike `AluOp`/`ShiftOp` below): `not`/`neg` share opcode `0xF7` but `inc`/`dec` use the unrelated `0xFF`, so there's no single stride pattern to abstract over, and forcing all four into one enum would obscure that split rather than reflect real shared structure.
2. `ShiftOp` (`Shl`/`Shr`/`Sar`) — genuinely shared structure across 3 operations (same opcode, extension-digit-only difference), matching `AluOp`'s justification for existing. Consumed by `shift_reg_imm8`/`shift_reg_cl`.
3. `lea_reg_mem` (trivial, mirrors `mov_reg_mem` exactly) and `lea_reg_scaled` — the substantial new piece: the first real SIB-with-index encoding in this crate, and the first real exercise of `rex()`'s `index` parameter (plumbed through since 6a, never actually used until now).
4. `imul128_reg`/`idiv_reg`/`cqo` — the RDX:RAX-implicit-operand instructions, none of which take a normal ModRM-selected destination the way every other instruction built so far does.

## Components

### Unary ops: `not_reg`/`neg_reg`/`inc_reg`/`dec_reg`

```rust
impl Assembler {
    /// `not dst` -- REX.W + F7 /2, one's complement in place.
    pub fn not_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(2, dst.encoding());
    }

    /// `neg dst` -- REX.W + F7 /3, two's complement negation in place.
    pub fn neg_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xF7);
        self.modrm_reg(3, dst.encoding());
    }

    /// `inc dst` -- REX.W + FF /0. In 64-bit mode the old single-byte
    /// INC opcodes (0x40-0x47) were repurposed as REX prefixes, so this
    /// ModRM-based form (group 5) is the only encoding that exists.
    pub fn inc_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xFF);
        self.modrm_reg(0, dst.encoding());
    }

    /// `dec dst` -- REX.W + FF /1. Same 64-bit-mode note as inc_reg.
    pub fn dec_reg(&mut self, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xFF);
        self.modrm_reg(1, dst.encoding());
    }
}
```

### `ShiftOp` and `shift_reg_imm8`/`shift_reg_cl`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShiftOp { Shl, Shr, Sar }

impl ShiftOp {
    /// The ModRM.reg opcode-extension digit (group 2). /6 is an unused
    /// alias slot, not implemented here.
    fn extension(self) -> u8 {
        match self {
            ShiftOp::Shl => 4,
            ShiftOp::Shr => 5,
            ShiftOp::Sar => 7,
        }
    }
}

impl Assembler {
    /// `op dst, shift` -- REX.W + C1 /n ib. NOTE: x86 masks the shift
    /// count to the low 6 bits for 64-bit operands at EXECUTION time --
    /// this method does not mask `shift` itself, it encodes whatever
    /// byte it's given. A caller passing 64 encodes literally 64, which
    /// the CPU then treats as a shift by 0 at runtime, not as "shift out
    /// everything."
    pub fn shift_reg_imm8(&mut self, op: ShiftOp, dst: PhysReg, shift: u8) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xC1);
        self.modrm_reg(op.extension(), dst.encoding());
        self.code.push(shift);
    }

    /// `op dst, cl` -- REX.W + D3 /n. Shift count comes from CL (the low
    /// byte of RCX); the caller is responsible for having loaded the
    /// count into CL beforehand.
    pub fn shift_reg_cl(&mut self, op: ShiftOp, dst: PhysReg) {
        self.rex(true, 0, 0, dst.encoding());
        self.code.push(0xD3);
        self.modrm_reg(op.extension(), dst.encoding());
    }
}
```

### `lea_reg_mem` and `lea_reg_scaled`

```rust
impl Assembler {
    /// `lea dst, [base + disp]` -- REX.W + 8D /r. Computes an address
    /// without dereferencing it. Reuses modrm_mem exactly like
    /// mov_reg_mem does, just with opcode 0x8D.
    pub fn lea_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        self.rex(true, dst.encoding(), 0, base.encoding());
        self.code.push(0x8D);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }

    /// `lea dst, [base + index*scale + disp]` -- REX.W + 8D /r with a
    /// real SIB byte (scale/index/base, not the "no index" pattern
    /// modrm_mem always emits for the rsp/r12 special case). First real
    /// use of rex()'s `index` parameter -- REX.X has been plumbed
    /// through since Phase 6a but never actually exercised until now.
    ///
    /// Two traps:
    ///   * `index == RSP` is architecturally unencodable: SIB.index=100
    ///     always means "no index," so there's no way to name RSP as a
    ///     scaled-index register. Asserted against.
    ///   * `base` in the rbp/r13 family (encoding low bits == 101) with
    ///     disp==0 still means RIP-relative unless forced to disp8, the
    ///     exact same trap modrm_mem handles for the base+disp-only
    ///     case -- it reapplies here identically, now combined with a
    ///     real index/scale in the SIB byte.
    pub fn lea_reg_scaled(
        &mut self,
        dst: PhysReg,
        base: PhysReg,
        index: PhysReg,
        scale: u8,
        disp: i32,
    ) {
        assert_ne!(
            index,
            PhysReg::Rsp,
            "RSP cannot be used as a scaled-index register -- x86 reserves \
             SIB.index=100 to mean \"no index\", so this combination is \
             architecturally unencodable"
        );
        let scale_bits = match scale {
            1 => 0b00,
            2 => 0b01,
            4 => 0b10,
            8 => 0b11,
            _ => panic!("scale must be 1, 2, 4, or 8, got {scale}"),
        };
        self.rex(true, dst.encoding(), index.encoding(), base.encoding());
        self.code.push(0x8D);
        let base_low = base.encoding() & 7;
        if base_low == 5 && disp == 0 {
            self.code.push(0b01 << 6 | ((dst.encoding() & 7) << 3) | 0b100);
            self.code.push(scale_bits << 6 | ((index.encoding() & 7) << 3) | base_low);
            self.code.push(0);
        } else {
            let mode = disp_mode(disp);
            self.code.push(mode.bits() << 6 | ((dst.encoding() & 7) << 3) | 0b100);
            self.code.push(scale_bits << 6 | ((index.encoding() & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }
}
```

### `imul128_reg`, `idiv_reg`, `cqo`

```rust
impl Assembler {
    /// `imul src` (one-operand form) -- RDX:RAX = RAX * src, signed,
    /// full 128-bit product. REX.W + F7 /5. Unlike every ModRM-based
    /// instruction so far, this has no explicit destination -- the
    /// result always lands in the implicit RDX:RAX pair.
    pub fn imul128_reg(&mut self, src: PhysReg) {
        self.rex(true, 0, 0, src.encoding());
        self.code.push(0xF7);
        self.modrm_reg(5, src.encoding());
    }

    /// `idiv src` -- RAX = RDX:RAX / src, RDX = RDX:RAX % src, signed.
    /// REX.W + F7 /7. PRECONDITION: RDX must already be the correct
    /// sign-extension of RAX (call cqo() first) -- otherwise this
    /// computes garbage or traps with #DE, even for divisions that
    /// don't actually overflow.
    pub fn idiv_reg(&mut self, src: PhysReg) {
        self.rex(true, 0, 0, src.encoding());
        self.code.push(0xF7);
        self.modrm_reg(7, src.encoding());
    }

    /// Sign-extends RAX into RDX:RAX -- the required precondition for
    /// idiv_reg. REX.W + 0x99. No ModRM, no operands -- the simplest
    /// instruction in this crate.
    pub fn cqo(&mut self) {
        self.rex(true, 0, 0, 0);
        self.code.push(0x99);
    }
}
```

## Testing

Same golden-byte + `iced-x86` round-trip discipline as 6a-6c, every disassembly string treated as an unverified guess until empirically confirmed. Required cases:

- `not_reg`/`neg_reg`/`inc_reg`/`dec_reg`: one test each.
- `ShiftOp`: one `shift_reg_imm8` test per operation (Shl/Shr/Sar, 3 tests), one `shift_reg_cl` test (any single operation — the CL form's opcode/extension-digit logic is identical across operations, already proven correct by the imm8 tests).
- `lea_reg_mem`: one test, confirming via disassembly that it's genuinely `lea` (computes an address) and not accidentally `mov` (dereferences) — same opcode-family risk `mov_mem_reg` vs. `mov_reg_mem` already established a precedent for checking.
- `lea_reg_scaled`: the highest-risk case in this slice. A real scaled-index test (e.g. `[rax + rbx*4]`), a `#[should_panic]` test confirming the RSP-as-index assert actually fires, and a test combining a real index with the rbp/r13-disp0 trap (e.g. base=rbp, disp=0, with a real index register) confirming the forced-disp8 rule still applies when a real SIB index is also present.
- `imul128_reg`/`idiv_reg`/`cqo`: one test each, confirming the ModRM-selected operand (where applicable) encodes correctly. There's no way to test the *implicit* RDX:RAX semantics via disassembly text beyond confirming the mnemonic and single operand — that limitation is inherent to what these instructions are, not a testing gap.

## Exit criteria

1. `not_reg`/`neg_reg`/`inc_reg`/`dec_reg` exist and pass tests.
2. `ShiftOp` and `shift_reg_imm8`/`shift_reg_cl` exist and pass tests for all 3 operations (imm8) plus at least one CL-form test.
3. `lea_reg_mem` exists and is tested to confirm it's genuinely `lea`, not `mov`.
4. `lea_reg_scaled` exists; a real scaled-index case, the RSP-as-index assert, and the combined rbp/r13-disp0-with-real-index case are all tested.
5. `imul128_reg`/`idiv_reg`/`cqo` exist and pass tests.
6. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
7. No regressions in 6a's/6b's/6c's existing tests or any other crate's tests.
