# Design: forge Phase 6b — x86-64 Arithmetic/Logic Instructions

**Status:** Approved for planning
**Scope:** The second sub-slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) — `mov`'s remaining forms (register/immediate load, memory store), the group-1 arithmetic/logic family (`add`/`or`/`and`/`sub`/`xor`, r/r and r/imm), and `imul` (two-operand and three-operand-immediate forms). This is what turns 6a's "can move bytes around" foundation into "can actually compute `a+b`, `a-b`, etc."
**Out of scope (deferred to later Phase 6 sub-slices):** `neg`/`not`/`inc`/`dec`, shifts (`shl`/`shr`/`sar`), `lea`, comparisons and conditional forms (`cmp`/`test`/`setcc`/`cmovcc`/`jcc`), the 128-bit `imul` form and `idiv` (magic-number division support), `push`/`pop`/`call`/`ret`, all of SSE2 scalar float, VEX/AVX, EVEX/AVX-512. Memory-operand forms of the arithmetic ops (`add r, [mem]`, etc.) are also out of scope — only r/r and r/imm, per CHECKLIST's literal wording for this bullet.

## Architecture

`crates/forge-x64/src/assembler.rs` grows three groups of methods, all built on 6a's existing private `rex()`/`modrm_reg()`/`modrm_mem()`/`DispMode` machinery — no new files, no changes to that foundation.

**Group 1 (`add`/`or`/`and`/`sub`/`xor`)** shares real x86 encoding structure: the same "ModRM.reg as opcode extension" trick for immediate forms, and r/r opcodes that differ only by a fixed per-operation offset. Rather than five-to-ten near-duplicate methods, a small `AluOp` enum carries each operation's opcode-extension digit and r/r opcode, consumed by two generic methods: `alu_reg_reg(op, dst, src)` and `alu_reg_imm(op, dst, imm: i32)` (auto-selecting the compact imm8 encoding when it fits, else imm32 — the same "encoder picks the smallest correct form" philosophy as 6a's `jmp` rel8/rel32 selection).

**`imul`** does not share group-1's shape (it's a genuinely different x86 encoding family with its own opcode bytes and operand direction) and gets its own two methods: `imul_reg_reg(dst, src)` (two-operand, `dst *= src`) and `imul_reg_reg_imm32(dst, src, imm)` (three-operand, non-destructive, `dst = src * imm`).

**`mov`** gets its remaining forms: `mov_reg_imm(dst, value: i64)` (auto-selecting the compact `C7 /0` form or the full 10-byte `movabs` form, mirroring the group-1 imm8/imm32 auto-selection) and `mov_mem_reg(base, disp, src)` (the store direction — the mirror image of 6a's `mov_reg_mem`, which only built the load direction).

## Components

### `AluOp`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AluOp { Add, Or, And, Sub, Xor }

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
        }
    }

    /// The direct r/r opcode (store-direction: ModRM.rm is the
    /// destination, ModRM.reg is the source -- same convention as
    /// `mov_reg_reg`'s 0x89).
    fn rr_opcode(self) -> u8 {
        match self {
            AluOp::Add => 0x01,
            AluOp::Or => 0x09,
            AluOp::And => 0x21,
            AluOp::Sub => 0x29,
            AluOp::Xor => 0x31,
        }
    }
}
```

(ADC=`/2` and SBB=`/3` exist in the same opcode family but are out of scope here — not part of CHECKLIST's requested instruction set.)

### `alu_reg_reg` / `alu_reg_imm`

```rust
impl Assembler {
    /// `op dst, src` -- e.g. `add rax, rbx`. REX.W + op.rr_opcode() /r,
    /// same shape as mov_reg_reg: ModRM.rm is the destination,
    /// ModRM.reg is the source.
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
}
```

### `imul`

```rust
impl Assembler {
    /// `imul dst, src` -- dst *= src. REX.W + 0F AF /r. Unlike the
    /// group-1 ops above, this is a LOAD-direction opcode: ModRM.reg is
    /// the destination, ModRM.rm is the source. This isn't a design
    /// choice -- it's the only two-operand IMUL r64,r/m64 encoding x86-64
    /// has. Do not copy alu_reg_reg's reg/rm assignment here.
    pub fn imul_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0xAF);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `imul dst, src, imm` -- dst = src * imm (three-operand,
    /// non-destructive). REX.W + 69 /r id. Same reg=dst/rm=src direction
    /// as imul_reg_reg (consistent with itself, still opposite to
    /// group-1's convention).
    pub fn imul_reg_reg_imm32(&mut self, dst: PhysReg, src: PhysReg, imm: i32) {
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x69);
        self.modrm_reg(dst.encoding(), src.encoding());
        self.code.extend_from_slice(&imm.to_le_bytes());
    }
}
```

### `mov_reg_imm`

```rust
impl Assembler {
    /// `mov dst, value` -- auto-selects the compact sign-extended-imm32
    /// form (REX.W + C7 /0 id) when `value` fits in i32, else the full
    /// 10-byte "movabs" form (REX.W + B8+rd io). The movabs form has NO
    /// ModRM byte at all -- the destination register is encoded directly
    /// into the low 3 bits of the opcode byte, with REX.B (not REX.R)
    /// covering register extension. Every instruction built in 6a had a
    /// ModRM byte; this is the first one that doesn't.
    pub fn mov_reg_imm(&mut self, dst: PhysReg, value: i64) {
        if let Ok(imm32) = i32::try_from(value) {
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xC7);
            self.modrm_reg(0, dst.encoding());
            self.code.extend_from_slice(&imm32.to_le_bytes());
        } else {
            self.rex(true, 0, 0, dst.encoding());
            self.code.push(0xB8 + (dst.encoding() & 7));
            self.code.extend_from_slice(&value.to_le_bytes());
        }
    }
}
```

### `mov_mem_reg`

```rust
impl Assembler {
    /// `mov [base + disp], src` -- 64-bit store. REX.W + 89 /r: the
    /// mirror image of mov_reg_mem (which uses 0x8B, load direction).
    /// Reuses modrm_mem directly, just swapping which operand is
    /// register vs. memory relative to mov_reg_mem's call shape.
    pub fn mov_mem_reg(&mut self, base: PhysReg, disp: i32, src: PhysReg) {
        self.rex(true, src.encoding(), 0, base.encoding());
        self.code.push(0x89);
        self.modrm_mem(src.encoding(), base.encoding(), disp);
    }
}
```

## Testing

Same two-layer discipline as 6a: golden-byte assertions plus `iced-x86` disassembler round-trip via the existing `disassemble()` harness in `tests/round_trip.rs`. Disassembly text strings must be empirically verified against a live compile before being committed, not guessed — 6a's plan found two real string-guessing mismatches this way (a hex-vs-decimal formatting surprise, confirmed only by running the tests).

Required cases:
- Each of the 5 `AluOp` variants gets at least one r/r test and at least one r/imm test; across the r/imm tests, both the imm8 and imm32 encoding paths must be exercised at least once each (not necessarily for every operation).
- `imul_reg_reg` and `imul_reg_reg_imm32`, with an explicit test confirming the reg/rm direction is right (dst and src don't silently swap) — e.g. using two registers with different encoding numbers and checking the disassembled operand order matches intent.
- `mov_reg_imm`: one case with a value fitting in i32 (compact form) and one that doesn't (movabs), and within the movabs cases, both a low register (no REX.B needed) and an extended register (e.g. R9, confirming REX.B still applies correctly even though there's no ModRM byte to normally carry that signal).
- `mov_mem_reg`: at least one test confirming it's genuinely the store direction via disassembly text (`mov [base+disp], src`, not `mov src, [base+disp]`) — not just golden bytes, since a reg/rm direction bug could plausibly produce a different-but-still-"valid-looking" encoding that only disassembly reveals as semantically wrong.

## Exit criteria

1. `AluOp`, `alu_reg_reg`, `alu_reg_imm` exist and pass both golden-byte and disassembler-round-trip tests for all 5 operations, with imm8 and imm32 both exercised.
2. `imul_reg_reg` and `imul_reg_reg_imm32` exist and pass tests explicitly confirming operand direction.
3. `mov_reg_imm` exists, auto-selects correctly, and is tested for both the compact and movabs paths, including an extended-register movabs case.
4. `mov_mem_reg` exists and is tested to confirm genuine store-direction semantics via disassembly.
5. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
6. No regressions in 6a's existing `forge-x64` tests or any other crate's tests.
