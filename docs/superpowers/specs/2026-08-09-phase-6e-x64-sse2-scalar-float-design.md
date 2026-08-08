# Design: forge Phase 6e — x86-64 SSE2 Scalar Float

**Status:** Approved for planning
**Scope:** The fifth sub-slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) — all of CHECKLIST's "SSE2 scalar float" bullet-group: `movsd`/`movapd`/`movq`, `addsd`/`subsd`/`mulsd`/`divsd`/`sqrtsd`, `minsd`/`maxsd`, `andpd`/`xorpd` (raw primitives, not composed abs/neg helpers), `ucomisd` reusing 6c's existing `ConditionCode`/`setcc`/`jcc`/`cmovcc` machinery unmodified, `cvtsi2sd`/`cvttsd2si`, and `roundsd`.
**Out of scope (deferred):** `VEX`/`AVX`, `EVEX`/`AVX-512`, `push`/`pop`/`call`/`ret` (still deferred to sit near Phase 7's prologue work, per 6c's scope decision), a constant pool with RIP-relative addressing (needed to materialize sign-mask constants for `abs`/`neg` cleanly — a distinct future task per SPEC.md), memory-operand `movapd` (register-register covers its most common real use; a small future addition if ever needed), `cvtsd2si` (the rounding conversion variant — CHECKLIST only asks for the truncating `cvttsd2si`).

This is where `PhysReg`'s `Xmm0`-`Xmm31` variants (declared in Phase 6a, never used until now) and `rex()`'s `index` parameter's sibling concept — a **mandatory legacy prefix byte** — finally get exercised for real.

## Architecture

All new methods live in `crates/forge-x64/src/assembler.rs`, reusing `rex()`/`modrm_reg()`/`modrm_mem()` exactly as before — those helpers operate purely on encoding numbers (0-15/0-31) and don't distinguish GPR from XMM identity. The one genuinely new mechanism: every SSE2/SSE4.1 instruction in this slice needs a mandatory legacy prefix byte (`0x66`, or `0xF2`) pushed **before** the REX byte — a real x86-64 prefix-ordering rule (REX must always be the byte immediately preceding the opcode) that nothing built in 6a-6d needed, since none of those instructions used a mandatory prefix.

`addsd`/`subsd`/`mulsd`/`divsd`/`sqrtsd`/`minsd`/`maxsd` share identical structure (same `0xF2` prefix, same `0x0F` escape, differing only in the final opcode byte, load-direction reg=dst/rm=src) — genuinely warranting an `SseOp` enum, the same justification `AluOp`/`ShiftOp` have from 6b/6d. `andpd`/`xorpd` share a prefix and escape but are only 2 operations, so — matching 6d's `not_reg`/`neg_reg` precedent — they stay as standalone methods. `ucomisd` reuses the *existing* `ConditionCode`/`setcc`/`jcc`/`cmovcc` machinery entirely as-is: float comparisons set flags the same way unsigned integer comparisons do, so no new condition-code concept is needed, just documenting which of the existing 16 codes are the semantically correct ones to use with a float comparison's results. `RoundMode` (`Nearest`/`Floor`/`Ceil`/`Truncate`) is a small closed-set enum for `roundsd`'s control byte, matching this project's established preference for descriptive enums over raw magic bytes.

## Components

### `movsd_reg_reg` / `movsd_reg_mem` / `movsd_mem_reg`

```rust
impl Assembler {
    /// `movsd dst, src` -- F2 0F 10 /r, load direction (reg=dst, rm=src).
    /// REX.W is always false -- unused/undefined for this opcode.
    pub fn movsd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x10);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `movsd dst, [base + disp]` -- F2 0F 10 /r, load direction, reuses
    /// modrm_mem exactly like mov_reg_mem/lea_reg_mem do.
    pub fn movsd_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, base.encoding());
        self.code.push(0x0F);
        self.code.push(0x10);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }

    /// `movsd [base + disp], src` -- F2 0F 11 /r, store direction, the
    /// mirror image of movsd_reg_mem (0x11 not 0x10).
    pub fn movsd_mem_reg(&mut self, base: PhysReg, disp: i32, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, src.encoding(), 0, base.encoding());
        self.code.push(0x0F);
        self.code.push(0x11);
        self.modrm_mem(src.encoding(), base.encoding(), disp);
    }
}
```

### `movapd_reg_reg`

```rust
impl Assembler {
    /// `movapd dst, src` -- 66 0F 28 /r. Register-register only -- its
    /// most common real use; a memory-operand form is a small future
    /// addition if ever needed, not built now.
    pub fn movapd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x28);
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

### `movq_gpr_to_xmm` / `movq_xmm_to_gpr`

```rust
impl Assembler {
    /// `movq dst(xmm), src(gpr)` -- 66 REX.W 0F 6E /r, load direction.
    /// REX.W matters here (and for movq_xmm_to_gpr/cvtsi2sd/cvttsd2si)
    /// since a real 64-bit GPR value is being moved -- unlike every
    /// other SSE2 method in this slice, where W is unused.
    pub fn movq_gpr_to_xmm(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x6E);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `movq dst(gpr), src(xmm)` -- 66 REX.W 0F 7E /r, store direction
    /// (rm=dst, reg=src) -- the mirror image of movq_gpr_to_xmm.
    pub fn movq_xmm_to_gpr(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(true, src.encoding(), 0, dst.encoding());
        self.code.push(0x0F);
        self.code.push(0x7E);
        self.modrm_reg(src.encoding(), dst.encoding());
    }
}
```

### `SseOp` and `sse_reg_reg`

```rust
/// A scalar-double arithmetic operation sharing identical F2-prefix,
/// 0F-escape structure, differing only by the final opcode byte -- the
/// same justification AluOp/ShiftOp have for existing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SseOp {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    Min,
    Max,
}

impl SseOp {
    fn opcode(self) -> u8 {
        match self {
            SseOp::Add => 0x58,
            SseOp::Sub => 0x5C,
            SseOp::Mul => 0x59,
            SseOp::Div => 0x5E,
            SseOp::Sqrt => 0x51,
            SseOp::Min => 0x5D,
            SseOp::Max => 0x5F,
        }
    }
}

impl Assembler {
    /// `op dst, src` -- F2 0F <op.opcode()> /r, load direction.
    /// minsd/maxsd are NOT commutative with respect to NaN (matching
    /// CHECKLIST's explicit warning and this project's interpreter's
    /// existing semantics) -- the encoder doesn't need to do anything
    /// special about this, but it's a real correctness fact worth
    /// documenting for whoever calls this with Min/Max.
    pub fn sse_reg_reg(&mut self, op: SseOp, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(op.opcode());
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

### `andpd_reg_reg` / `xorpd_reg_reg`

```rust
impl Assembler {
    /// `andpd dst, src` -- 66 0F 54 /r. Raw bitwise-AND primitive, used
    /// (by a caller, not this method) to implement float `abs` by
    /// clearing the sign bit against a materialized sign-mask constant.
    /// This method does NOT materialize any mask itself -- that's
    /// instruction-selection's job (mov_reg_imm + movq_gpr_to_xmm),
    /// matching this crate's established "thin composable primitives"
    /// philosophy (see idiv_reg's cqo precondition, setcc's undone
    /// zero-extension).
    pub fn andpd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x54);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `xorpd dst, src` -- 66 0F 57 /r. Same raw-primitive philosophy as
    /// andpd_reg_reg, used to implement float `neg` by flipping the sign
    /// bit against a materialized mask.
    pub fn xorpd_reg_reg(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x57);
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

### `ucomisd_reg_reg`

```rust
impl Assembler {
    /// `ucomisd a, b` -- 66 0F 2E /r. Compares `a` and `b`, sets EFLAGS.
    ///
    /// IMPORTANT: ucomisd sets ZF/PF/CF the same way an UNSIGNED integer
    /// `cmp` does, not the SF/OF-based signed comparison flags. Use the
    /// unsigned ConditionCode variants with setcc/jcc/cmovcc after this
    /// (Below/BelowOrEqual/Above/AboveOrEqual/Equal/NotEqual), NOT the
    /// signed ones (Less/LessOrEqual/Greater/GreaterOrEqual) -- using the
    /// signed codes after a float comparison produces a plausible-looking
    /// but wrong result. No changes are needed in setcc/jcc/cmovcc
    /// themselves; this is purely a caller-facing usage note.
    pub fn ucomisd_reg_reg(&mut self, a: PhysReg, b: PhysReg) {
        self.code.push(0x66);
        self.rex(false, a.encoding(), 0, b.encoding());
        self.code.push(0x0F);
        self.code.push(0x2E);
        self.modrm_reg(a.encoding(), b.encoding());
    }
}
```

### `cvtsi2sd` / `cvttsd2si`

```rust
impl Assembler {
    /// `cvtsi2sd dst(xmm), src(gpr)` -- F2 REX.W 0F 2A /r, load direction
    /// (reg=dst xmm, rm=src gpr). REX.W selects the 64-bit GPR source
    /// form (forge's i64), matching the AAPCS64/SysV convention this
    /// project always widens to.
    pub fn cvtsi2sd(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x2A);
        self.modrm_reg(dst.encoding(), src.encoding());
    }

    /// `cvttsd2si dst(gpr), src(xmm)` -- F2 REX.W 0F 2C /r, load
    /// direction with the GPR as ModRM.reg (the destination) this time --
    /// direction is opposite to cvtsi2sd's, a real place to get backward.
    /// Truncating (toward zero), NOT rounding -- cvtsd2si (a different
    /// opcode, 0x2D) is the rounding variant and isn't built here.
    pub fn cvttsd2si(&mut self, dst: PhysReg, src: PhysReg) {
        self.code.push(0xF2);
        self.rex(true, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x2C);
        self.modrm_reg(dst.encoding(), src.encoding());
    }
}
```

### `RoundMode` and `roundsd`

```rust
/// The four rounding modes CHECKLIST asks for (floor/ceil/round/trunc).
/// `roundsd`'s control byte also always sets bit 3 (0x08, "suppress
/// precision exception") -- the standard convention every mainstream
/// compiler uses, since without it a rounding operation that loses
/// precision raises a floating-point exception most code doesn't want.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundMode {
    Nearest,
    Floor,
    Ceil,
    Truncate,
}

impl RoundMode {
    fn control_byte(self) -> u8 {
        let mode = match self {
            RoundMode::Nearest => 0x00,
            RoundMode::Floor => 0x01,
            RoundMode::Ceil => 0x02,
            RoundMode::Truncate => 0x03,
        };
        mode | 0x08
    }
}

impl Assembler {
    /// `roundsd dst, src, mode` -- 66 0F 3A 0B /r ib. SSE4.1, not pure
    /// SSE2 (CHECKLIST's own bullet notes this) -- a 3-byte opcode
    /// (0F 3A escape + 0B) plus an immediate control byte, the most
    /// novel encoding shape in this slice. Runtime CPUID feature
    /// detection for SSE4.1 availability is a separate, later concern
    /// (this task only builds the encoder).
    pub fn roundsd(&mut self, mode: RoundMode, dst: PhysReg, src: PhysReg) {
        self.code.push(0x66);
        self.rex(false, dst.encoding(), 0, src.encoding());
        self.code.push(0x0F);
        self.code.push(0x3A);
        self.code.push(0x0B);
        self.modrm_reg(dst.encoding(), src.encoding());
        self.code.push(mode.control_byte());
    }
}
```

## Testing

Same golden-byte + `iced-x86` round-trip discipline as 6a-6d, every disassembly string treated as unverified until empirically confirmed — 6a-6d found real guessing mistakes nearly every time this was checked, no reason to expect SSE2's mnemonic naming or operand formatting to be more predictable. Required cases:

- `movsd`: load (reg-reg), store, and one memory-operand test (reusing `modrm_mem`, no need to re-prove its rsp/rbp/r12/r13 traps — already exhaustively covered in 6a, opcode-agnostic).
- `movapd`: one register-register test proving the `0x66` prefix mechanism works.
- `movq`: a direction-check pair (xmm←gpr via `0x6E`, gpr←xmm via `0x7E`), confirming they aren't swapped.
- `SseOp`: at least one test per operation (7 tests) — this is the first real proof each individual opcode byte (`58`/`5C`/`59`/`5E`/`51`/`5D`/`5F`) is correct, not just that the generic mechanism works.
- `andpd`/`xorpd`: one test each.
- `ucomisd`: one test confirming the encoding, plus one test demonstrating the unsigned-condition-code usage in practice (encode `ucomisd` followed by `setcc(Below, ...)`, confirming the combination disassembles sensibly) — as close as a disassembly-only suite can get to proving the semantic claim in the doc comment.
- `cvtsi2sd`/`cvttsd2si`: one test each, confirming REX.W is set and the reg/rm direction is correct for each (GPR is `reg` for one, `rm` for the other — a real place to get backward).
- `roundsd`: one test per `RoundMode` variant (4 tests), confirming the control-byte-plus-precision-bit math (e.g. `Floor` → `0x01 | 0x08 = 0x09`) — the one place in this slice where an off-by-one bit error could silently select the wrong rounding mode.

## Exit criteria

1. `movsd_reg_reg`/`movsd_reg_mem`/`movsd_mem_reg` exist and pass tests, including a memory-operand case.
2. `movapd_reg_reg` and `movq_gpr_to_xmm`/`movq_xmm_to_gpr` exist; the movq pair's direction is tested.
3. `SseOp` and `sse_reg_reg` exist and pass tests for all 7 operations.
4. `andpd_reg_reg`/`xorpd_reg_reg` exist and pass tests.
5. `ucomisd_reg_reg` exists, is tested, and its unsigned-condition-code usage is demonstrated in a test combining it with `setcc`.
6. `cvtsi2sd`/`cvttsd2si` exist; both direction and REX.W are tested.
7. `RoundMode` and `roundsd` exist and pass tests for all 4 modes.
8. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
9. No regressions in 6a's/6b's/6c's/6d's existing tests or any other crate's tests.
