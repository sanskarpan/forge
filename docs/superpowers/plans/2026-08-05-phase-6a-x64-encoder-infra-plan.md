# forge Phase 6a x86-64 Encoder Infrastructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `forge-x64`'s foundational encoder machinery — `PhysReg`, `Assembler` (REX/ModRM/SIB emission with the mandatory rsp/rbp/r12/r13 special cases, label/fixup-based jump resolution) — verified via a real disassembler-round-trip oracle (`iced-x86`), not hand-inspection alone.

**Architecture:** `crates/forge-x64/src/reg.rs` holds `PhysReg`; `crates/forge-x64/src/assembler.rs` holds `Assembler`/`Label`/`Fixup` and all encoding logic. `iced-x86` is a `[dev-dependencies]`-only dependency of `forge-x64`, used exclusively from `crates/forge-x64/tests/round_trip.rs` (a separate compilation unit from `src/`), so it is structurally impossible for the disassembler oracle to leak into a non-test path. Two minimal instructions (`mov_reg_reg`, `mov_reg_mem`) and one control-flow instruction (`jmp`) exist solely to drive real, disassembler-verified tests of the REX/ModRM/SIB/fixup machinery — the rest of the x86-64 instruction set is out of scope for this plan.

**Tech Stack:** Rust, `iced-x86` 1.21 (dev-dependency, disassembler oracle only — see PROMPT.md's explicit rule that it never appears in a non-test path).

**Design doc:** `docs/superpowers/specs/2026-08-05-phase-6a-x64-encoder-infra-design.md` — read this first.

---

## Task 1: `PhysReg` + `iced-x86` dev-dependency wiring

**Files:**
- Create: `crates/forge-x64/src/reg.rs`
- Modify: `crates/forge-x64/src/lib.rs` (currently a one-line placeholder — overwrite it)
- Modify: `crates/forge-x64/Cargo.toml`

- [ ] **Step 1: Add `iced-x86` as a dev-dependency**

```toml
# crates/forge-x64/Cargo.toml — full file contents

[package]
name = "forge-x64"
version.workspace = true
edition.workspace = true

[dev-dependencies]
iced-x86.workspace = true
```

Note there is deliberately **no** `[dependencies]` section — `forge-x64` has no runtime dependencies at this stage, and `iced-x86` must never become one (see PROMPT.md: "`iced-x86` is a test oracle only... never in a non-test path").

- [ ] **Step 2: Write the failing test**

```rust
// crates/forge-x64/src/reg.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpr_encodings_match_hardware_numbers() {
        assert_eq!(PhysReg::Rax.encoding(), 0);
        assert_eq!(PhysReg::Rdi.encoding(), 7);
        assert_eq!(PhysReg::R8.encoding(), 8);
        assert_eq!(PhysReg::R15.encoding(), 15);
    }

    #[test]
    fn xmm_encodings_match_hardware_numbers() {
        assert_eq!(PhysReg::Xmm0.encoding(), 0);
        assert_eq!(PhysReg::Xmm15.encoding(), 15);
        assert_eq!(PhysReg::Xmm31.encoding(), 31);
    }

    #[test]
    fn needs_rex_is_true_exactly_for_encoding_8_and_above() {
        assert!(!PhysReg::Rdi.needs_rex()); // encoding 7
        assert!(PhysReg::R8.needs_rex()); // encoding 8
        assert!(!PhysReg::Rax.needs_rex()); // encoding 0
        assert!(!PhysReg::Xmm7.needs_rex());
        assert!(PhysReg::Xmm8.needs_rex());
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -20`
Expected: FAIL — `PhysReg` not defined.

- [ ] **Step 3: Write the implementation above the test module**

```rust
// crates/forge-x64/src/reg.rs — above the `#[cfg(test)]` module

/// A physical x86-64 register: all 16 general-purpose registers and all 32
/// XMM slots (XMM16-31 need EVEX to reach and can't be used by anything
/// built so far -- their encoding numbers are still real data worth
/// representing now, since gating actual usability is an AVX-512-era
/// concern, not a `PhysReg` concern).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysReg {
    Rax, Rcx, Rdx, Rbx, Rsp, Rbp, Rsi, Rdi,
    R8, R9, R10, R11, R12, R13, R14, R15,
    Xmm0, Xmm1, Xmm2, Xmm3, Xmm4, Xmm5, Xmm6, Xmm7,
    Xmm8, Xmm9, Xmm10, Xmm11, Xmm12, Xmm13, Xmm14, Xmm15,
    Xmm16, Xmm17, Xmm18, Xmm19, Xmm20, Xmm21, Xmm22, Xmm23,
    Xmm24, Xmm25, Xmm26, Xmm27, Xmm28, Xmm29, Xmm30, Xmm31,
}

impl PhysReg {
    /// The hardware encoding number: 0-15 for GPRs, 0-31 for XMM. GPRs and
    /// XMM registers share the same 0-15 (or 0-31) numbering space --
    /// distinguishing "GPR 0" from "XMM 0" is the caller's job (which
    /// opcode/ModRM.reg-or-rm slot this number goes into), not this type's.
    pub fn encoding(self) -> u8 {
        use PhysReg::*;
        match self {
            Rax => 0, Rcx => 1, Rdx => 2, Rbx => 3,
            Rsp => 4, Rbp => 5, Rsi => 6, Rdi => 7,
            R8 => 8, R9 => 9, R10 => 10, R11 => 11,
            R12 => 12, R13 => 13, R14 => 14, R15 => 15,
            Xmm0 => 0, Xmm1 => 1, Xmm2 => 2, Xmm3 => 3,
            Xmm4 => 4, Xmm5 => 5, Xmm6 => 6, Xmm7 => 7,
            Xmm8 => 8, Xmm9 => 9, Xmm10 => 10, Xmm11 => 11,
            Xmm12 => 12, Xmm13 => 13, Xmm14 => 14, Xmm15 => 15,
            Xmm16 => 16, Xmm17 => 17, Xmm18 => 18, Xmm19 => 19,
            Xmm20 => 20, Xmm21 => 21, Xmm22 => 22, Xmm23 => 23,
            Xmm24 => 24, Xmm25 => 25, Xmm26 => 26, Xmm27 => 27,
            Xmm28 => 28, Xmm29 => 29, Xmm30 => 30, Xmm31 => 31,
        }
    }

    /// Whether addressing this register on its own merits (independent of
    /// REX.W or any other operand) requires a REX prefix -- true for any
    /// encoding number >= 8 (r8-r15, xmm8-xmm31).
    pub fn needs_rex(self) -> bool {
        self.encoding() >= 8
    }
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod reg;

pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -20`
Expected: 3 tests pass.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/Cargo.toml crates/forge-x64/src/reg.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): PhysReg + iced-x86 dev-dependency wiring"
```

## Context for this task

`forge-x64` currently has only a one-line placeholder `src/lib.rs` (`//! Stub crate — not yet implemented...`) and an empty `[dependencies]` section in `Cargo.toml`. This task is the first real code in the crate. `PhysReg` has no dependency on `iced-x86` at all — this task's tests are pure unit tests with no disassembler involved; wiring `iced-x86` as a dev-dependency here just gets the Cargo.toml change landed early so later tasks don't need to touch it.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `Assembler` skeleton + displacement-mode helpers

**Files:**
- Create: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/forge-x64/src/assembler.rs

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
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -20`
Expected: FAIL — `Assembler` not defined.

- [ ] **Step 3: Write the implementation above the test module**

```rust
// crates/forge-x64/src/assembler.rs — above the `#[cfg(test)]` module

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
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::Assembler;
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -20`
Expected: 3 (from Task 1) + 8 (new) = 11 tests pass.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): Assembler skeleton + displacement-mode helpers"
```

## Context for this task

These helpers are pure byte-manipulation with no disassembler needed to verify — every test here just checks exact bytes, which is fully trustworthy on its own for logic this mechanical (there's no ambiguity to verify empirically, unlike a real instruction's opcode/mnemonic). The label/fixup fields (`labels`, `fixups`) are deliberately NOT added to `Assembler` in this task — they'd sit unused until Task 5 adds `jmp()`, which would trip `cargo clippy -- -D warnings`'s dead-code lint. Task 5 adds those fields and their machinery together, exercised immediately by real tests in the same commit.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `rex()` + `modrm_reg()` + `mov_reg_reg()`, with the disassembler round-trip harness

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Create: `crates/forge-x64/tests/round_trip.rs`

This task introduces `iced-x86` for the first time, in `tests/round_trip.rs` — a separate integration-test binary from `src/`, so `iced-x86` structurally cannot leak into the library itself (confirm this by checking `cargo tree -p forge-x64` after this task shows `iced-x86` only under the `[dev-dependencies]` tree, not the main dependency tree).

- [ ] **Step 1: Write the failing test**

```rust
// crates/forge-x64/tests/round_trip.rs

use forge_x64::{Assembler, PhysReg};
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, NasmFormatter};

/// Assembles into a fresh, disposable buffer and returns each decoded
/// instruction's formatted text, in order. This is the project's test
/// oracle for "did we encode what we meant to encode" -- see PROMPT.md's
/// rule that `iced-x86` never appears outside a test path (this whole file
/// is a `tests/` integration test binary, compiled separately from `src/`,
/// so that rule is structurally enforced, not just followed by convention).
///
/// NOTE: if this doesn't compile against the installed iced-x86 version,
/// check `cargo doc -p iced-x86 --open` for the current Decoder/Formatter
/// API -- this was written from the crate's documented usage pattern, not
/// verified against a live compile.
fn disassemble(bytes: &[u8]) -> Vec<String> {
    let mut decoder = Decoder::with_ip(64, bytes, 0, DecoderOptions::NONE);
    let mut formatter = NasmFormatter::new();
    let mut result = Vec::new();
    let mut instruction = Instruction::default();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let mut text = String::new();
        formatter.format(&instruction, &mut text);
        result.push(text);
    }
    result
}

#[test]
fn mov_reg_reg_needs_rex_b_for_an_extended_destination() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::R12, PhysReg::Rax); // dst=r12 (needs REX.B), src=rax
    assert_eq!(a.code(), &[0x49, 0x89, 0xC4]);
    assert_eq!(disassemble(a.code()), vec!["mov r12,rax"]);
}

#[test]
fn mov_reg_reg_still_emits_rex_w_when_no_other_rex_bit_is_needed() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::Rbx, PhysReg::Rax); // neither register needs REX.R/X/B
    assert_eq!(a.code(), &[0x48, 0x89, 0xC3]);
    assert_eq!(disassemble(a.code()), vec!["mov rbx,rax"]);
}

#[test]
fn mov_reg_reg_needs_rex_r_for_an_extended_source() {
    let mut a = Assembler::new();
    a.mov_reg_reg(PhysReg::Rax, PhysReg::R9); // dst=rax, src=r9 (needs REX.R)
    assert_eq!(a.code(), &[0x4C, 0x89, 0xC8]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,r9"]);
}
```

**IMPORTANT — before trusting the exact disassembly text above**: `NasmFormatter`'s exact output (spacing, whether it's `"mov r12,rax"` or `"mov r12, rax"`) depends on the installed `iced-x86` version's default formatter options, which was not verified against a live compile. Run this test once with the byte-comparison assertions only (comment out or temporarily remove the `disassemble(...)` assertions), confirm the golden bytes pass first, then add a `println!("{:?}", disassemble(a.code()));` (or run with `cargo test -p forge-x64 --test round_trip -- --nocapture`) to observe the ACTUAL formatted string iced-x86 produces, and correct the `assert_eq!` string literals in this file to match what you observe — do not guess or leave the string as the placeholder above without checking. This mirrors the project's established "verify empirically" discipline for anything involving an external tool's exact output format.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 2>&1 | head -30`
Expected: FAIL — `mov_reg_reg` not defined (and the crate won't compile).

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block containing `emit_disp`

impl Assembler {
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
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -30`
Expected: all pass (11 lib tests from Tasks 1-2 + 3 new integration tests). Remember Step 1's instruction: verify the `disassemble(...)` string literals empirically before trusting this "pass" — if you skipped that verification, go back and do it now.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Confirm `iced-x86` is dev-only**

Run: `cargo tree -p forge-x64 --edges normal` (should NOT list `iced-x86`) and `cargo tree -p forge-x64 --edges dev` (SHOULD list `iced-x86`).

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): rex/modrm_reg + mov_reg_reg, disassembler round-trip harness"
```

## Context for this task

The `rex()`/`modrm_reg()` logic here is transcribed directly from SPEC.md §8.2 (this project's own source-of-truth doc) — it's already correctly reasoned, not something to redesign. Your job is faithful transcription plus real, disassembler-verified proof it works, not reinvention. The three test cases were hand-derived and cross-checked against known x86-64 encoding references (`mov r12,rax` = `49 89 C4`, `mov rbx,rax` = `48 89 C3`, `mov rax,r9` = `4C 89 C8`) — if your empirical run disagrees with these exact byte sequences, that's a real bug to investigate (in `rex()`/`modrm_reg()`/`mov_reg_reg()`), not something to paper over by changing the expected bytes.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 4: `modrm_mem()` + `mov_reg_mem()`, covering all four ModRM special cases

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

**Note (found during Task 2):** `disp_mode()`, `emit_disp()`, and `DispMode::bits()` currently carry a temporary `#[allow(dead_code)]` each — plain `cargo clippy --workspace -- -D warnings` (this project's actual CI invocation, no `--all-targets`) can't see their `#[cfg(test)]`-only call sites, so without the allow it reports them as unused. `modrm_mem()` below is their first production call site (note it calls `mode.bits()`, not the raw `DispMode` value, when building the ModRM byte) — remove all three `#[allow(dead_code)]` attributes as part of this task, once `modrm_mem()` calls them for real.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn mov_reg_mem_generic_base_with_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rcx, 8);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x41, 0x08]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rcx+8]"]);
}

#[test]
fn mov_reg_mem_generic_base_with_disp32() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rcx, 1000);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x81, 0xE8, 0x03, 0x00, 0x00]);
    // NasmFormatter renders larger displacements in hex with a trailing
    // "h", not decimal -- confirmed empirically, not guessed.
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rcx+3E8h]"]);
}

/// rsp requires a SIB byte -- ModRM.rm=100 alone means "SIB follows", so
/// `[rsp]` cannot be encoded without one.
#[test]
fn mov_reg_mem_rsp_base_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rsp, 0);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x04, 0x24]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rsp]"]);
}

/// r12 hits the SAME SIB-required case as rsp, via REX.B -- easy to
/// handle rsp and forget its extended twin.
#[test]
fn mov_reg_mem_r12_base_requires_sib() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R12, 0);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x04, 0x24]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r12]"]);
}

/// rbp with disp=0 must use mod=01 disp8=0 -- mod=00 rm=101 means
/// RIP-relative, not `[rbp]`.
#[test]
fn mov_reg_mem_rbp_base_with_zero_disp_forces_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::Rbp, 0);
    assert_eq!(a.code(), &[0x48, 0x8B, 0x45, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[rbp]"]);
}

/// r13 hits the SAME disp0 trap as rbp, via REX.B.
#[test]
fn mov_reg_mem_r13_base_with_zero_disp_forces_disp8() {
    let mut a = Assembler::new();
    a.mov_reg_mem(PhysReg::Rax, PhysReg::R13, 0);
    assert_eq!(a.code(), &[0x49, 0x8B, 0x45, 0x00]);
    assert_eq!(disassemble(a.code()), vec!["mov rax,[r13]"]);
}
```

Per Task 3's note: verify these `disassemble(...)` string literals empirically (run with `--nocapture` and observe, or temporarily comment out the disassembly assertions to confirm the golden bytes pass first) before trusting them — the exact spacing/format iced-x86's `NasmFormatter` produces for memory operands (e.g. whether it's `[rcx+8]` or `[rcx+0x8]`) was not verified against a live compile.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `mov_reg_mem` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — add to the `impl Assembler` block

impl Assembler {
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
            self.code.push(mode.bits() << 6 | ((reg & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }

    /// `mov dst, [base + disp]` -- 64-bit load. Encoded as
    /// `REX.W + 8B /r` (MOV r64, r/m64): ModRM.reg is the destination,
    /// ModRM.rm (via `modrm_mem`) addresses the memory operand.
    pub fn mov_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) {
        self.rex(true, dst.encoding(), 0, base.encoding());
        self.code.push(0x8B);
        self.modrm_mem(dst.encoding(), base.encoding(), disp);
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -40`
Expected: all pass (11 lib tests + 9 integration tests: 3 from Task 3 + 6 new).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): modrm_mem + mov_reg_mem, covering rsp/rbp/r12/r13 special cases"
```

## Context for this task

This is the highest-risk task in the plan — `modrm_mem`'s special cases are exactly what CHECKLIST.md calls out as the traps most likely to be gotten wrong (and, per the checklist's own wording, "very easy to handle rsp/rbp and forget their extended twins r12/r13"). All six test cases here were hand-derived and cross-checked against known x86-64 encoding references. If any empirical disassembly disagrees with the golden bytes given, that is very likely a real bug in `modrm_mem` (most plausibly: forgetting the `base_low` masking so r12/r13 don't get routed into the same branches as rsp/rbp) — investigate the encoder, don't adjust the golden bytes to match broken output.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 5: Labels, fixups, and `jmp` with rel8/rel32 selection

**Files:**
- Modify: `crates/forge-x64/src/assembler.rs`
- Modify: `crates/forge-x64/src/lib.rs`
- Modify: `crates/forge-x64/tests/round_trip.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/tests/round_trip.rs — append

#[test]
fn backward_jump_that_fits_uses_rel8() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler: 48 89 C0
    let len_before_jmp = a.code().len();
    a.jmp(l); // backward reference, close enough for rel8

    let expected_rel = -(len_before_jmp as i32 + 2); // rel8 measured from the end of this 2-byte instruction
    assert_eq!(a.code()[len_before_jmp], 0xEB);
    assert_eq!(a.code()[len_before_jmp + 1], expected_rel as i8 as u8);
    assert_eq!(a.code().len(), len_before_jmp + 2);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with("jmp"));
}

#[test]
fn backward_jump_that_does_not_fit_uses_rel32() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l); // label at position 0
    for _ in 0..50 {
        a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes each, 150 bytes total -- far enough that rel8 can't reach
    }
    let len_before_jmp = a.code().len();
    a.jmp(l); // backward reference, too far for rel8

    let expected_rel = -(len_before_jmp as i32 + 5); // rel32 measured from the end of this 5-byte instruction
    assert_eq!(a.code()[len_before_jmp], 0xE9);
    assert_eq!(
        &a.code()[len_before_jmp + 1..len_before_jmp + 5],
        &expected_rel.to_le_bytes()
    );
    assert_eq!(a.code().len(), len_before_jmp + 5);

    let text = disassemble(a.code());
    assert!(text.last().unwrap().starts_with("jmp"));
}

#[test]
fn forward_jump_always_uses_rel32() {
    let mut a = Assembler::new();
    let l = a.new_label();
    let jmp_at = a.code().len(); // 0
    a.jmp(l); // forward reference -- label not bound yet
    assert_eq!(a.code()[jmp_at], 0xE9); // always rel32 for forward jumps, never rel8
    assert_eq!(a.code().len(), jmp_at + 5);

    a.mov_reg_reg(PhysReg::Rax, PhysReg::Rax); // 3 bytes of filler between the jmp and its target
    let target_pos = a.code().len();
    a.bind(l); // resolves the fixup recorded above

    let expected_rel = target_pos as i32 - (jmp_at as i32 + 5); // rel32 measured from the end of the 5-byte jmp
    assert_eq!(&a.code()[jmp_at + 1..jmp_at + 5], &expected_rel.to_le_bytes());

    let text = disassemble(a.code());
    assert!(text[0].starts_with("jmp"));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --test round_trip 2>&1 | head -30`
Expected: FAIL — `new_label`/`bind`/`jmp` not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-x64/src/assembler.rs — modify the Assembler struct definition

/// Emits x86-64 machine code byte by byte, tracking label positions and
/// pending forward-jump fixups.
pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

/// An opaque handle to a not-yet-necessarily-bound code position, created
/// by `Assembler::new_label` and resolved by `Assembler::bind`.
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
```

```rust
// crates/forge-x64/src/assembler.rs — update Assembler::new() to initialize the new fields

impl Assembler {
    pub fn new() -> Self {
        Self { code: Vec::new(), labels: Vec::new(), fixups: Vec::new() }
    }

    // ... code() stays unchanged ...

    /// Creates a new, not-yet-bound label.
    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Records the label's address as the current end of `code`, then
    /// resolves every pending fixup that targets it by patching the
    /// placeholder displacement bytes reserved at fixup-creation time.
    pub fn bind(&mut self, label: Label) {
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
```

- [ ] **Step 4: Update `lib.rs` to export `Label`**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod reg;

pub use assembler::{Assembler, Label};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 2>&1 | tail -50`
Expected: all pass (14 lib tests + 16 integration tests: 13 from Tasks 3-4 + 3 new). Task 4's own review added 4 extra `mov_reg_mem` layering tests beyond its original plan text, and Task 1's review added an extra `PhysReg` test beyond its original plan text — both are reflected in these higher counts, which are the real, current numbers, not the plan's original estimates.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/assembler.rs crates/forge-x64/src/lib.rs crates/forge-x64/tests/round_trip.rs
git commit -m "feat(forge-x64): label/fixup machinery + jmp with rel8/rel32 selection"
```

## Context for this task

The three test cases deliberately express their expected relative-offset bytes as a **formula computed from `a.code().len()` at each observation point** rather than hand-derived magic numbers — this is more robust than hardcoding exact hex bytes (which would require precisely hand-counting 150 filler bytes in the rel32 case) and just as rigorous, since the formula IS the target-address check the design doc calls for ("each verified by disassembling and checking the resolved target address, not just which opcode form was chosen"). If a test fails, first double check the formula reasoning in the failing assertion (are you measuring "distance from the end of the jump instruction," which is what x86-64's rel8/rel32 actually encode, not distance from the start?) before assuming the encoder itself is wrong.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 6: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including `forge-x64`'s new tests (14 lib unit tests + 16 integration tests in `tests/round_trip.rs` — higher than this plan's original per-task estimates due to extra tests added during Tasks 1's and 4's own reviews; see those tasks' final commits for the authoritative counts). No regressions in the pre-existing 164 tests from Phases 0-5.

- [ ] **Step 2: Confirm `iced-x86` never appears in a non-test path**

Run: `cargo tree -p forge-x64 --edges normal` (must NOT list `iced-x86` or anything pulling it in) and `cargo tree -p forge-x64 --edges dev` (must list `iced-x86`).

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 4: Format check**

Run: `cargo fmt --check`

- [ ] **Step 5: Confirm the day-one spike and Phase 5's forge-mem tests still work (this phase shouldn't have touched them)**

Run: `make spike` and `cargo test -p forge-mem 2>&1 | tail -20`

- [ ] **Step 6: Report exit criteria status**

Confirm all 6 exit criteria from the design doc are met:
1. `PhysReg`, `Assembler`, `Label`/`Fixup`, and the REX/ModRM/SIB helpers exist and match SPEC.md §8.2's design. ✅ (Tasks 1-5)
2. `mov_reg_reg`, `mov_reg_mem`, and `jmp` exist and pass both golden-byte and disassembler-round-trip tests. ✅ (Tasks 3-5)
3. All four ModRM special cases (rsp, rbp+disp0, r12, r13+disp0) are tested explicitly and pass. ✅ (Task 4)
4. Backward-rel8, backward-rel32, and forward-rel32 jump cases are tested explicitly and pass, including target-address verification. ✅ (Task 5)
5. `iced-x86` appears only in `forge-x64`'s `[dev-dependencies]`, never in a non-test path. ✅ (Task 3, re-confirmed Step 2)
6. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 1, 3, 4)
