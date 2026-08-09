# Design: forge Phase 6f — x86-64 Calling Convention & RIP-Relative Addressing

**Status:** Approved for planning
**Scope:** The sixth and final sub-slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) needed before Phase 7 can start — the last two still-unbuilt 🔴-blocking items: `push`/`pop`/`call`/`ret` (deferred since 6c's scope decision) and RIP-relative addressing (deferred since 6a's original design, needed for Phase 7's f64 constant pool).
**Out of scope (deferred):** `VEX`/`AVX` (🟡 important, no consumer before Phase 10 — SIMD Vectorization) and `EVEX`/`AVX-512` (🔵 stretch, same reasoning) — both remain deferred with no committed date; a full constant-pool *system* (layout, dedup, placement-after-code) — Phase 7 owns that, this slice only builds the RIP-relative addressing *primitive* it will need; `cvtsd2si` (rounding conversion, already out of scope per 6e); `push`/`pop` of immediates or memory operands (no consumer — Phase 7's callee-saved save/restore only ever pushes/pops a register); the `ret imm16` stack-cleanup form (not used by SysV or Win64).

## Architecture

All new methods live in `crates/forge-x64/src/assembler.rs`, in new `impl Assembler` blocks appended at the end of the file, matching this file's established one-block-per-instruction-family convention. Two independent mechanisms are introduced:

**Calling-convention instructions** reuse `rex()`/`modrm_reg()` exactly as before. `push_reg`/`pop_reg` are opcode-plus-register-in-low-3-bits forms (`0x50+r`/`0x58+r`) with no ModRM at all — the same shape `mov_reg_imm` already uses for its opcode byte, just without an immediate. `call_reg` is a group-5 extension-digit instruction (`FF /2`), the same idiom `inc_reg`/`idiv_reg` already established in 6d. `call_rel32` reuses `Label`/`Fixup`/`bind()` from 6a exactly like `jmp`, but is simpler: call has no rel8 short form, so it's unconditionally the 5-byte `E8 rel32` form regardless of whether the label is bound yet. `ret` is a bare opcode byte, no operands, no ModRM — the simplest instruction in this file alongside `cqo`.

**RIP-relative addressing** is a new addressing mode, not a new instruction family. In 64-bit mode, ModRM `mod=00, rm=101` is a *fixed bit pattern* meaning "RIP-relative, disp32 follows" — it does not derive from any real base register's encoding, so REX.B must never be set for it (unlike every other ModRM form in this file, where the `rm` argument is a real register's encoding number). The key insight making this cheap to build: the trailing disp32 is the *last bytes of the instruction* for both of this slice's consumers (`lea_reg_riprel`, `movsd_reg_riprel` — neither has a trailing immediate after its memory operand), so "relative to the address of the next instruction" is exactly the same computation `bind()` already performs for `jmp`/`jcc`/`call_rel32`'s fixups. No new fixup kind, no new patching logic — just two new call sites recording a `Fixup` the same way `jmp`'s forward-jump branch already does.

## Components

### `push_reg` / `pop_reg`

```rust
impl Assembler {
    /// `push src` -- 50+r, no ModRM (register encoded in the opcode's
    /// low 3 bits, same shape mov_reg_imm's opcode byte uses). No
    /// REX.W -- push/pop default to 64-bit operand size in long mode;
    /// REX.W has no defined effect on this opcode. REX.B only if src
    /// is r8-r15.
    pub fn push_reg(&mut self, src: PhysReg) {
        self.rex(false, 0, 0, src.encoding());
        self.code.push(0x50 + (src.encoding() & 7));
    }

    /// `pop dst` -- 58+r, the mirror image of push_reg.
    pub fn pop_reg(&mut self, dst: PhysReg) {
        self.rex(false, 0, 0, dst.encoding());
        self.code.push(0x58 + (dst.encoding() & 7));
    }
}
```

### `call_reg` / `call_rel32` / `ret`

```rust
impl Assembler {
    /// `call target` -- FF /2, indirect through a register holding an
    /// absolute address. This is how forge calls libm: mov_reg_imm the
    /// function pointer into a register, then call_reg through it --
    /// a direct rel32 call can't reach an arbitrary libm address
    /// reliably (it may be outside +/-2GiB of the JIT buffer).
    pub fn call_reg(&mut self, target: PhysReg) {
        self.rex(false, 0, 0, target.encoding());
        self.code.push(0xFF);
        self.modrm_reg(2, target.encoding());
    }

    /// `call label` -- E8 rel32, direct call within the same code
    /// buffer (e.g. a future JIT-to-JIT call). Unlike jmp/jcc there is
    /// no rel8 short form for call at all -- this is unconditionally
    /// the 5-byte form, whether the label is already bound (backward,
    /// distance computed immediately) or not (forward, recorded as a
    /// Fixup exactly like jmp's forward-jump branch).
    pub fn call_rel32(&mut self, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end = self.code.len() + 5;
            let rel32 = target_pos as isize - end as isize;
            self.code.push(0xE8);
            self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
        } else {
            self.code.push(0xE8);
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]);
            self.fixups.push(Fixup { at, target: label });
        }
    }

    /// `ret` -- C3, no operands. The rare imm16 stack-cleanup form
    /// (stdcall-style callee cleanup) is not used by SysV or Win64 and
    /// isn't built.
    pub fn ret(&mut self) {
        self.code.push(0xC3);
    }
}
```

### `lea_reg_riprel` / `movsd_reg_riprel`

```rust
impl Assembler {
    /// `lea dst, [rip + disp32]` where disp32 resolves label -- REX.W +
    /// 8D /r with a fixed mod=00/rm=101 ModRM byte. REX.B is
    /// deliberately never set here (rex() is called with rm=0) since
    /// mod=00/rm=101 is not a real register's encoding -- it's the
    /// CPU's dedicated RIP-relative bit pattern in 64-bit mode, and
    /// setting REX.B would be meaningless/undefined for it. Reuses
    /// lea_reg_mem's REX.W=true and opcode 0x8D.
    pub fn lea_reg_riprel(&mut self, dst: PhysReg, label: Label) {
        self.rex(true, dst.encoding(), 0, 0);
        self.code.push(0x8D);
        self.code.push(0b00_000_101 | ((dst.encoding() & 7) << 3));
        self.riprel_fixup(label);
    }

    /// `movsd dst, [rip + disp32]` -- F2 0F 10 /r with the same
    /// mod=00/rm=101 RIP-relative ModRM shape as lea_reg_riprel.
    /// Reuses movsd_reg_reg's F2 prefix, REX.W=false, and opcode 0x10.
    pub fn movsd_reg_riprel(&mut self, dst: PhysReg, label: Label) {
        self.code.push(0xF2);
        self.rex(false, dst.encoding(), 0, 0);
        self.code.push(0x0F);
        self.code.push(0x10);
        self.code.push(0b00_000_101 | ((dst.encoding() & 7) << 3));
        self.riprel_fixup(label);
    }

    /// Shared by both RIP-relative methods above: emits the trailing
    /// disp32 (immediately, if `label` is already bound -- an unusual
    /// case for a constant pool placed after the code, but handled for
    /// correctness/symmetry with jmp/call_rel32) or a 4-byte
    /// placeholder plus a Fixup (the expected case, since Phase 7's
    /// constant pool is placed after the code it's referenced from).
    /// Correct ONLY when disp32 is the last bytes of the instruction --
    /// true for both current callers, since neither has a trailing
    /// immediate after its memory operand. A future RIP-relative
    /// consumer WITH a trailing immediate (e.g. an imm32 ALU op reading
    /// a RIP-relative operand) would need a different fixup scheme;
    /// document this constraint rather than generalizing prematurely.
    fn riprel_fixup(&mut self, label: Label) {
        if let Some(target_pos) = self.labels[label.0] {
            let end = self.code.len() + 4;
            let rel32 = target_pos as isize - end as isize;
            self.code.extend_from_slice(&(rel32 as i32).to_le_bytes());
        } else {
            let at = self.code.len();
            self.code.extend_from_slice(&[0, 0, 0, 0]);
            self.fixups.push(Fixup { at, target: label });
        }
    }
}
```

## Testing

Same golden-byte + `iced-x86` round-trip discipline as 6a-6e, every disassembly string treated as unverified until empirically confirmed. Required cases:

- `push_reg`/`pop_reg`: one low-register test each, plus one extended-register test each (e.g. `R12`, proving REX.B threads through correctly with no ModRM byte involved at all — a genuinely different shape than every prior REX test in this file, which all paired REX with a ModRM byte).
- `call_reg`: one low-register and one extended-register test, confirming the `FF /2` extension digit is correct (not `/0`-`/7`'s sibling group-5/group-3 operations already built in 6d).
- `call_rel32`: a forward-reference test and a backward-reference test, mirroring `jmp`'s existing test structure exactly (formulaic assertion on the patched rel32 value, not a hand-derived literal) — but with only one length to check (5 bytes), since call has no short form to disambiguate.
- `ret`: one test, trivial single-byte assertion.
- `lea_reg_riprel`/`movsd_reg_riprel`: one forward-reference test each (the realistic case — the constant pool comes after the code), asserting the ModRM byte's fixed `mod=00/rm=101` pattern, the formulaic patched disp32 value (matching `jmp`'s established assertion style), and the `iced-x86` disassembly showing a `[rip+...]`-style operand. — **correction (Phase 6f, Task 3):** the `[rip+...]` form above was an unverified guess, per this section's own opening disclaimer. Empirically confirmed against a live `iced-x86` disassembly, `NasmFormatter` actually renders a RIP-relative operand as `[rel <resolved absolute target>]` — a NASM-syntax convention (NASM has no literal `rip` register token; RIP-relative operands are written with the `rel` keyword instead), not a relative-offset `[rip+N]` form. The byte-level and formulaic-offset assertions this bullet also requires are what actually prove the encoding correct either way, and needed no correction.

## Exit criteria

1. `push_reg`/`pop_reg` exist and pass tests, including an extended-register case for each.
2. `call_reg` (indirect) and `call_rel32` (direct, via `Label`) both exist and pass tests, including a forward and backward reference for `call_rel32`.
3. `ret` exists and passes a test.
4. `lea_reg_riprel`/`movsd_reg_riprel` exist, pass tests, and correctly reuse the `Label`/`Fixup`/`bind()` machinery from 6a with no changes to `bind()`, `Fixup`, or `patch_fixup()` themselves.
5. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
6. No regressions in 6a-6e's existing tests or any other crate's tests.
7. CHECKLIST.md's `push`/`pop`/`call`/`ret` bullet and the "RIP-relative addressing for constant pool loads" bullet are annotated to reflect what was actually built in this slice, matching the note/correction pattern used at the end of every prior Phase 6 sub-slice.
