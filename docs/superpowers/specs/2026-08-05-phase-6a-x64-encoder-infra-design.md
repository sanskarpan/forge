# Design: forge Phase 6a — x86-64 Encoder Infrastructure

**Status:** Approved for planning
**Scope:** The foundational slice of CHECKLIST.md Phase 6 ("x86-64 Encoder," 40 tasks total) — `PhysReg`, the `Assembler` struct, REX/ModRM/SIB emission (including the mandatory rsp/rbp/r12/r13 special cases), label/fixup machinery with rel8/rel32 jump selection, and the disassembler-round-trip test harness that every later encoding task depends on. Two minimal `mov` forms and one `jmp` exist solely to exercise this infrastructure through real, disassembler-verified test cases — not as general-purpose instruction coverage.
**Out of scope (deferred to later Phase 6 sub-slices):** the rest of `mov`'s forms (imm32/imm64/m→r other sizes), all other scalar integer instructions (add/sub/imul/shifts/lea/cmp/setcc/cmovcc/push/pop/call/ret/jcc), all SSE2 scalar float instructions, VEX/AVX, EVEX/AVX-512. Phase 6 is being decomposed into several design→plan→implementation cycles rather than one, given its size (40 tasks vs. 10-32 for prior phases); this document covers only the first.

This slice replaces `forge-x64`'s placeholder `src/lib.rs` with real encoder infrastructure. Nothing built here is directly useful to `CompiledExpr` yet — the milestone is "the trap-heavy foundation (REX/ModRM/SIB/labels) is correct and independently verified," which every later instruction-emitter task then builds on without needing to re-litigate.

## Architecture

`forge-x64` gets a `PhysReg` enum (all GPRs and XMM registers, encoding numbers baked in), an `Assembler` struct (`code: Vec<u8>`, `labels: Vec<Option<usize>>`, `fixups: Vec<Fixup>`), and the private REX/ModRM/SIB helper functions from SPEC.md §8.2, transcribed faithfully — that code is already correct, hand-designed reference material embedded in the project's own source-of-truth spec, not something to redesign from scratch.

`iced-x86` is added to `forge-x64`'s `[dev-dependencies]` specifically (it's currently only a workspace-level dependency declaration in the root `Cargo.toml`; this task adds the actual `[dev-dependencies]` entry to `crates/forge-x64/Cargo.toml`) — never `[dependencies]` — so it's structurally impossible for the disassembler oracle to leak into a non-test codegen path, matching PROMPT.md's explicit rule ("iced-x86 is a test oracle only... never in a non-test path").

Two minimal `mov` forms (register←register, register←[base+disp]) and one `jmp` (rel8/rel32, forward/backward) exist purely to give the ModRM/SIB/REX/fixup logic something real to round-trip test against via `iced-x86`. The rest of the instruction set — everything else in CHECKLIST.md's Phase 6 — is out of scope here and belongs to later sub-slices (6b: scalar integer, 6c: SSE2 float, 6d: VEX/AVX).

## Components

### `PhysReg`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysReg {
    // GPRs, encoding numbers 0-15
    Rax, Rcx, Rdx, Rbx, Rsp, Rbp, Rsi, Rdi,
    R8, R9, R10, R11, R12, R13, R14, R15,
    // XMM, encoding numbers 0-31 (16-31 need EVEX to reach -- out of
    // scope until AVX-512 lands, but the encoding numbers themselves are
    // just data and cost nothing to represent now)
    Xmm0, Xmm1, /* ... */ Xmm31,
}

impl PhysReg {
    /// The 4-or-5-bit hardware encoding number (0-15 for GPRs, 0-31 for XMM).
    pub fn encoding(self) -> u8 { /* ... */ }
    /// Whether addressing this register requires a REX prefix on its own
    /// merits (encoding >= 8), independent of REX.W or other operands.
    pub fn needs_rex(self) -> bool { self.encoding() >= 8 }
}
```

### `Assembler`, `Label`, `Fixup`

```rust
pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(usize);

#[derive(Clone, Copy, Debug)]
enum FixupKind { Rel8, Rel32 }

struct Fixup {
    /// Byte offset in `code` where the displacement bytes go (i.e., right
    /// after the opcode, not the start of the instruction).
    at: usize,
    target: Label,
    kind: FixupKind,
}

impl Assembler {
    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Records the label's address as the current end of `code`, then
    /// resolves every pending fixup that targets it by writing the real
    /// displacement into the already-reserved bytes at `fixup.at`.
    pub fn bind(&mut self, label: Label) { /* ... */ }
}
```

`bind()` only ever *resolves* fixups recorded for *forward* references (the label wasn't bound yet when the jump to it was emitted) — it never rewrites an already-emitted rel8/rel32 *choice*, since that choice is fixed at emit time per the jump policy below. There is no byte-shifting or promotion-after-the-fact anywhere in this design.

### Jump policy: rel8/rel32 selection without promotion

- **Backward jump** (target label already bound at emit time): the real byte distance is known immediately, so the encoder picks rel8 if it fits in `i8`, else rel32 — encoded directly, no fixup needed at all, since nothing about it can change later.
- **Forward jump** (target label not yet bound): the real distance isn't knowable until `bind()` runs later, and true "promote in place" would require shifting every byte after the insertion point and adjusting every later label position and pending fixup — real complexity with real cascading-reflow risk, for a JIT whose functions are small expressions, not something a code-size-sensitive AOT compiler needs. So forward jumps unconditionally emit rel32 and record a `Fixup { kind: Rel32, .. }`, resolved once `bind()` runs. This is CHECKLIST.md's "rel8 vs rel32 jump selection" satisfied honestly (both forms exist and are chosen correctly, backward jumps do get the tighter encoding when possible) without the byte-shifting machinery "automatic promotion" would imply for the forward case.

### REX / ModRM / SIB

Transcribed from SPEC.md §8.2:

```rust
impl Assembler {
    fn rex(&mut self, w: bool, reg: u8, index: u8, rm: u8) {
        let byte = 0x40
            | ((w as u8) << 3)
            | (((reg   >> 3) & 1) << 2)   // REX.R
            | (((index >> 3) & 1) << 1)   // REX.X
            |  ((rm    >> 3) & 1);        // REX.B
        if byte != 0x40 { self.code.push(byte); }
    }

    fn modrm_reg(&mut self, reg: u8, rm: u8) {
        self.code.push(0b11 << 6 | ((reg & 7) << 3) | (rm & 7));
    }

    fn modrm_mem(&mut self, reg: u8, base: u8, disp: i32) {
        let base_low = base & 7;
        if base_low == 4 {                       // RSP or R12 -> SIB required
            let mode = disp_mode(disp);
            self.code.push(mode << 6 | ((reg & 7) << 3) | 0b100);
            self.code.push(0b00_100_100);        // scale=1, index=none, base=rsp/r12
            self.emit_disp(mode, disp);
        } else if base_low == 5 && disp == 0 {   // RBP or R13 -> must use disp8
            self.code.push(0b01 << 6 | ((reg & 7) << 3) | base_low);
            self.code.push(0);                   // explicit zero displacement
        } else {
            let mode = disp_mode(disp);
            self.code.push(mode << 6 | ((reg & 7) << 3) | base_low);
            self.emit_disp(mode, disp);
        }
    }
}
```

`rex()`'s omit-when-unneeded logic, `modrm_mem()`'s rsp/r12-needs-SIB and rbp/r13-disp0-means-RIP-relative special cases, are preserved exactly as SPEC.md documents them — this is the trap-heavy core the CHECKLIST calls out repeatedly, and it's already correctly reasoned in the project's own source-of-truth doc.

### The two bootstrap instructions

```rust
impl Assembler {
    /// mov dst, src  (register-to-register, 64-bit)
    pub fn mov_reg_reg(&mut self, dst: PhysReg, src: PhysReg) { /* REX.W + 0x89 + modrm_reg */ }

    /// mov dst, [base + disp]  (64-bit load)
    pub fn mov_reg_mem(&mut self, dst: PhysReg, base: PhysReg, disp: i32) { /* REX.W + 0x8B + modrm_mem */ }

    /// jmp label  (unconditional, rel8 or rel32 per the policy above)
    pub fn jmp(&mut self, label: Label) { /* ... */ }
}
```

These three methods exist only to drive the round-trip tests below. The full `mov` opcode family and all other instructions are Phase 6b+.

## Testing

Two complementary layers, matching CHECKLIST.md's explicit requirements:

1. **Golden-byte tests** — hand-derive the expected exact hex bytes for a handful of encodings (mirroring the day-one spike's `48 89 F8 C3` style) and `assert_eq!` against `Assembler`'s internal byte buffer.
2. **Disassembler round-trip tests** — assemble, then disassemble via `iced-x86`'s formatter, then compare the resulting mnemonic text to what was intended. This is the primary oracle, per PROMPT.md's rule.

Required cases, each covered by both layers where practical:
- Register-direct ModRM (`mov_reg_reg`).
- Generic memory ModRM (a base register that isn't rsp/rbp/r12/r13).
- The **rsp** SIB-required case, and its extended twin **r12** (via REX.B) — both must be tested explicitly, not just one, per CHECKLIST's explicit warning that r12/r13 are easy to forget.
- The **rbp with disp=0** forced-disp8 case, and its extended twin **r13**.
- A REX.W-vs-no-REX size difference (confirming REX.W is actually being asserted for 64-bit forms).
- A register ≥8 (e.g. R9, R13) needing REX.R or REX.B on its own merits, independent of REX.W.
- Labels: a backward jump close enough for rel8, a backward jump far enough to require rel32, and a forward jump (always rel32 per the policy) — each verified by disassembling and checking the resolved target address, not just which opcode form was chosen.

## Exit criteria

1. `PhysReg`, `Assembler`, `Label`/`Fixup`, and the REX/ModRM/SIB helpers exist and match SPEC.md §8.2's design.
2. `mov_reg_reg`, `mov_reg_mem`, and `jmp` exist and pass both golden-byte and disassembler-round-trip tests.
3. All four ModRM special cases (rsp, rbp+disp0, r12, r13+disp0) are tested explicitly and pass.
4. Backward-rel8, backward-rel32, and forward-rel32 jump cases are tested explicitly and pass, including target-address verification via disassembly.
5. `iced-x86` appears only in `forge-x64`'s `[dev-dependencies]`, never in a non-test code path — enforced structurally by Cargo's dependency graph, not just by convention.
6. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
