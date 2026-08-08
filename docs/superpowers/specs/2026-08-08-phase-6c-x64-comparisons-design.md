# Design: forge Phase 6c — x86-64 Comparisons and Conditional Branches

**Status:** Approved for planning
**Scope:** The third sub-slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) — `cmp`, `test`, `setcc`, `cmovcc`, and `jcc` (conditional jump — pulled out of CHECKLIST's `push`/`pop`/`call`/`ret`/`jcc` bullet and grouped here instead, since it's the coherent unit of capability forge's IR actually needs: `if`/`else` compiles to a comparison plus a conditional branch, and CHECKLIST's own bullet boundaries don't reflect that). All 16 x86-64 condition codes are implemented now, not just the 6 forge's current signed-integer comparisons need — see "Condition codes" below.
**Out of scope (deferred to later Phase 6 sub-slices):** `neg`/`not`/`inc`/`dec`, shifts (`shl`/`shr`/`sar`), `lea`, the 128-bit `imul` form and `idiv` (magic-number division support), `push`/`pop`/`call`/`ret` (real calling-convention work, closer in spirit to Phase 7's "Instruction Selection & Prologue"), all of SSE2 scalar float, VEX/AVX, EVEX/AVX-512.

forge's `if`/`else` construct (already built in Phase 0-3's SSA IR with φ-nodes — see SPEC.md's SSA construction section) needs a real conditional branch to compile at all; comparisons-as-values (`setcc` alone) aren't sufficient on their own. This slice makes that possible.

## Architecture

`crates/forge-x64/src/assembler.rs` gains a `ConditionCode` enum (all 16 condition codes, each carrying its 4-bit "cc" nibble, reused across `setcc`/`cmovcc`/`jcc`'s opcode computation). `cmp` is added as a new `AluOp` variant (`Cmp`, extension=7, r/r opcode=0x39) rather than its own type — it turns out to fit group-1's exact encoding shape (structurally identical to `sub`, just discarding the result), so `alu_reg_reg(AluOp::Cmp, ...)` and `alu_reg_imm(AluOp::Cmp, ...)` work with zero new encoder logic, just one new enum variant. `test` gets its own two methods (`test_reg_reg`, `test_reg_imm`) since its opcodes (`0x85`, `0xF7 /0`) don't fit group-1's stride. `setcc`, `cmovcc`, `jcc` each take a `ConditionCode` parameter. `setcc` introduces one genuinely new piece of machinery — a REX-prefix-forcing rule for byte-sized destinations (see below). `jcc` reuses 6a's `jmp`/`Fixup`/`bind`/`patch_fixup` machinery unmodified, adjusted only for `jcc`'s own different instruction lengths.

## Condition codes

All 16, matching Intel's canonical ordering — the nibble value is the low 4 bits of the corresponding `Jcc`/`SETcc`/`CMOVcc` opcode byte:

| Nibble | Variant | Meaning |
|---|---|---|
| 0 | `Overflow` | OF=1 |
| 1 | `NotOverflow` | OF=0 |
| 2 | `Below` | CF=1 (unsigned <) |
| 3 | `AboveOrEqual` | CF=0 (unsigned >=) |
| 4 | `Equal` | ZF=1 |
| 5 | `NotEqual` | ZF=0 |
| 6 | `BelowOrEqual` | CF=1 or ZF=1 (unsigned <=) |
| 7 | `Above` | CF=0 and ZF=0 (unsigned >) |
| 8 | `Sign` | SF=1 |
| 9 | `NotSign` | SF=0 |
| 10 | `Parity` | PF=1 |
| 11 | `NotParity` | PF=0 |
| 12 | `Less` | SF≠OF (signed <) |
| 13 | `GreaterOrEqual` | SF=OF (signed >=) |
| 14 | `LessOrEqual` | ZF=1 or SF≠OF (signed <=) |
| 15 | `Greater` | ZF=0 and SF=OF (signed >) |

forge's current i64 comparisons only need `Equal`/`NotEqual`/`Less`/`GreaterOrEqual`/`LessOrEqual`/`Greater` (6 of the 16), but all 16 are implemented now per your scope decision — the other 10 (unsigned, sign, overflow, parity) will matter once forge grows unsigned or float comparisons.

## Components

### `AluOp::Cmp`

```rust
// Add to the existing AluOp enum from Phase 6b:
pub enum AluOp {
    Add, Or, And, Sub, Xor,
    Cmp, // NEW: extension=7, rr_opcode=0x39
}
```

`extension()` gains `AluOp::Cmp => 7`; `rr_opcode()` gains `AluOp::Cmp => 0x39`. Both `alu_reg_reg`/`alu_reg_imm` need no changes at all.

### `test_reg_reg` / `test_reg_imm`

```rust
impl Assembler {
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
```

### `ConditionCode` and `setcc`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConditionCode {
    Overflow, NotOverflow, Below, AboveOrEqual, Equal, NotEqual,
    BelowOrEqual, Above, Sign, NotSign, Parity, NotParity,
    Less, GreaterOrEqual, LessOrEqual, Greater,
}

impl ConditionCode {
    fn nibble(self) -> u8 {
        use ConditionCode::*;
        match self {
            Overflow => 0, NotOverflow => 1, Below => 2, AboveOrEqual => 3,
            Equal => 4, NotEqual => 5, BelowOrEqual => 6, Above => 7,
            Sign => 8, NotSign => 9, Parity => 10, NotParity => 11,
            Less => 12, GreaterOrEqual => 13, LessOrEqual => 14, Greater => 15,
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
    /// full-width 0/1 is instruction-selection's job (e.g. an `xor`
    /// before this call), not this method's.
    pub fn setcc(&mut self, cc: ConditionCode, dst: PhysReg) {
        self.rex_for_byte_dst(dst.encoding());
        self.code.push(0x0F);
        self.code.push(0x90 + cc.nibble());
        self.modrm_reg(0, dst.encoding());
    }
}
```

### `cmovcc`

```rust
impl Assembler {
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
```

### `jcc`

```rust
impl Assembler {
    /// `jcc label` -- conditional jump. Mirrors jmp's rel8/rel32
    /// auto-selection and Fixup reuse exactly, except for length:
    /// the short form is 2 bytes (70+cc, rel8), the near form is 6
    /// bytes (0F 80+cc, rel32) -- one byte longer than jmp's 5-byte
    /// near form, since the conditional opcode is 2 bytes, not 1.
    /// patch_fixup() needs no changes: it only depends on fixup.at
    /// (the position of the 4 placeholder bytes), not on how long the
    /// preceding opcode was.
    pub fn jcc(&mut self, cc: ConditionCode, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end_if_short = self.code.len() + 2;
            let rel = target_pos as isize - end_if_short as isize;
            if let Ok(rel8) = i8::try_from(rel) {
                self.code.push(0x70 + cc.nibble());
                self.code.push(rel8 as u8);
            } else {
                let end_if_near = self.code.len() + 6;
                let rel32 = target_pos as isize - end_if_near as isize;
                self.code.push(0x0F);
                self.code.push(0x80 + cc.nibble());
                self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
            }
        } else {
            self.code.push(0x0F);
            self.code.push(0x80 + cc.nibble());
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]);
            self.fixups.push(Fixup { at, target: label });
        }
    }
}
```

## Testing

Same golden-byte + `iced-x86` round-trip discipline as 6a/6b, with every disassembly string treated as an unverified guess until empirically confirmed against a live compile — 6a and 6b both found real guessing mistakes (hex-vs-decimal rendering, a resolved mnemonic-naming question) nearly every time this was checked, so this discipline is not optional. Given 16 condition codes across several instructions, exhaustive per-instruction-per-condition-code testing would be excessive; required cases:

- `AluOp::Cmp`: one r/r and one r/imm test, confirming it works via the existing `alu_reg_reg`/`alu_reg_imm` machinery with no encoder changes.
- `test_reg_reg` and `test_reg_imm`: one test each, including the `test_reg_reg(x, x)` self-test zero-check idiom.
- `setcc`: dedicated REX-forcing coverage — one destination in encoding 0-3 (no REX expected), one in encoding 4-7 (the trap case: must disassemble to the spl/bpl/sil/dil-family name, not silently succeed while actually meaning ah/ch/dh/bh), one extended (8-15, REX already mandatory via the normal path). At least 2-3 distinct condition codes used across the whole `setcc`/`cmovcc`/`jcc` test set (not `Equal` everywhere), including at least one non-trivial nibble value, to catch an off-by-one in the `90+cc`/`40+cc`/`70+cc`/`80+cc` arithmetic.
- `cmovcc`: one direction-check test (mirroring `imul_reg_reg`'s pair from 6b), confirming reg=dst/rm=src.
- `jcc`: backward-short, backward-near, and forward-near cases (mirroring `jmp`'s three cases from 6a), plus explicit confirmation via golden bytes that the near form is genuinely 6 bytes, not `jmp`'s 5.

## Exit criteria

1. `AluOp::Cmp` exists and passes r/r and r/imm tests via the existing machinery.
2. `test_reg_reg` and `test_reg_imm` exist and pass tests, including the zero-check idiom.
3. `ConditionCode` (all 16 variants) and `setcc` exist; the byte-register REX-forcing rule is tested for encodings 0-3, 4-7, and 8-15 specifically.
4. `cmovcc` exists and passes a direction-check test.
5. `jcc` exists and passes backward-short, backward-near, and forward-near tests, with the 6-byte near-form length explicitly confirmed.
6. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
7. No regressions in 6a's or 6b's existing tests or any other crate's tests.
