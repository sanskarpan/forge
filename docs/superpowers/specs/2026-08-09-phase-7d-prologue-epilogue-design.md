# Design: forge Phase 7d — Prologue/Epilogue & ABI Frame Plumbing

**Status:** Approved for planning
**Scope:** The fourth sub-slice of CHECKLIST.md Phase 7 — four bullets: "Prologue," "Stack alignment," "Callee-saved register save/restore," and "Epilogue." Builds `emit_prologue`/`emit_epilogue`: real byte-emitting functions (calling `Assembler` methods directly — the first Phase 7 work operating at the encoder layer, not the `MachineInst`-selection layer 7a-7c built), parameterized by `(callee_saved: &[PhysReg], spill_bytes: u32)` and tested with hand-picked synthetic values, per this project's established resolution to Phase 7/8's circular dependency (Phase 7 builds parameterized plumbing; Phase 8's real register-allocation output becomes the real input once it exists).
**Out of scope (deferred):** "Red zone (System V): 128 bytes below rsp usable without adjustment in leaf functions" — this is a future *optimization* (skip prologue/epilogue entirely for a leaf function needing ≤128 bytes of scratch), not a correctness requirement; nothing in this slice needs it, and detecting "is this function a leaf needing ≤128 bytes" is naturally a decision for whatever future pass actually calls `emit_prologue` with real data. "Win64 shadow space: 32 bytes allocated by the caller before any call" — this is about the *call site* (forge's generated code, as a caller, allocating shadow space before calling libm on Win64), not about forge's own function prologue; it belongs to Phase 7e's libm call sequence, not this slice. **Win64 support entirely** — deferred; see "Why System V only" below.

## Why System V only, not System V + Win64

CHECKLIST's callee-saved bullet doesn't specify an ABI, and 7a-7c's `MachineInst` design is deliberately ABI-agnostic (a future emission step reads `callee_saved`/`spill_bytes` as plain inputs). But building this slice for BOTH ABIs isn't just "duplicate the constants" — Win64's callee-saved set includes `XMM6`-`XMM15` (System V has no callee-saved XMM registers at all: "All XMM registers are caller-saved in System V" per SPEC.md), and there is no `push`/`pop` for an XMM register on x86-64 — saving one needs a `movsd`/`movups`-to-stack-memory sequence instead, a genuinely different code path from the GPR case, not a parameter change. Checking this project's own CI matrix (Phase 0's bootstrap task): `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu` (QEMU), `wasm32-unknown-unknown` — **no Windows CI target exists**. (Windows platform support IS planned elsewhere — `forge-mem`'s platform detection and a future `docs/PLATFORMS.md` both mention it — so this isn't "Windows is out of scope for the project," just "untestable in this project's actual CI today.") Building real, tested Win64 XMM-save support for an ABI this project can't exercise in CI right now would be speculative generality with no way to verify it's correct. `emit_prologue`/`emit_epilogue`'s actual code doesn't hardcode System V anywhere (they just push/pop whatever `PhysReg`s they're given), so nothing here forecloses Win64 support later if a real need arises — this slice just doesn't build or test it now.

## Architecture

New file `crates/forge-x64/src/prologue.rs` (the first Phase 7 file that isn't `machine_inst.rs`/`machine_inst/` — this is real encoder-layer work, calling `Assembler` methods directly to produce real bytes, unlike 7a-7c's virtual-register `MachineInst` selection). Exported from `lib.rs` alongside the existing `assembler`/`machine_inst`/`reg` modules.

**The critical correctness detail this slice is really about**: `mov rsp, rbp` (the "`leave`-equivalent shortcut" CHECKLIST's epilogue bullet mentions) is only a valid epilogue when there are ZERO callee-saved registers to restore. Once any callee-saved register was pushed in the prologue, `mov rsp, rbp` would silently *skip* restoring its value — it just moves the stack pointer past the saved data without ever popping it back into the register, which is a correctness bug (violates SPEC.md §18's "preserve all callee-saved registers" property), not merely a style choice. The correct epilogue instead: `add rsp, N` (discard spill space) → pop each callee-saved register **in reverse push order** (this is what actually restores their values, and as a side effect also walks `rsp` back to exactly where it was right after `push rbp`) → `pop rbp` → `ret`. No `mov rsp, rbp` anywhere. This is simpler than it might sound, and — a nice property — it degrades correctly to the simple 2-instruction case (`pop rbp; ret`) when `callee_saved` is empty and `spill_bytes` is 0, without needing a special-cased "shortcut" branch at all.

**Stack alignment**: at function entry, `rsp ≡ 8 (mod 16)` — the caller's `rsp` was 16-aligned before `call`, and `call` pushes an 8-byte return address. After `push rbp`, `rsp` is back to 16-aligned (`8 + 8 = 16`). After `K` more `push_reg` calls for callee-saved registers (8 bytes each), `rsp`'s offset from 16-aligned is `(K*8) mod 16` — `0` if `K` is even, `8` if `K` is odd. The subsequent `sub rsp, N` must choose `N ≥ requested_spill_bytes` such that the total is a multiple of 16, so the *first* `call` instruction inside the function body (Phase 7e's libm calls) finds `rsp` already 16-aligned. This is computed by a small shared pure function, called independently by both `emit_prologue` and `emit_epilogue` with the same inputs — guaranteeing they always agree on the padded size without threading a computed value between two separate call sites (the same "one shared pure function, not two independent implementations" discipline 7b's `match_scaled_index` established).

**`Rbp` must never appear in the `callee_saved` slice** — its save/restore is unconditionally baked into the `push rbp`/`pop rbp` frame-pointer steps themselves. Passing `Rbp` in `callee_saved` would double-save/double-restore it (a real, easy-to-make caller mistake) — both functions assert against this loudly (`assert!`, not `debug_assert!`, matching this project's established "caller bugs must fail loudly in release too" precedent from 6a's `bind()`).

## Components

```rust
/// Callee-saved GPRs per System V AMD64 -- does NOT include Rbp, whose
/// save/restore is handled unconditionally by emit_prologue/emit_epilogue
/// themselves, never by the caller passing it in this list.
pub const SYSV_CALLEE_SAVED: &[PhysReg] =
    &[PhysReg::Rbx, PhysReg::R12, PhysReg::R13, PhysReg::R14, PhysReg::R15];

/// Computes the actual byte count `sub rsp, N` / `add rsp, N` should use:
/// `requested`, padded up to the next value making the TOTAL frame
/// (callee-saved pushes + this) a multiple of 16 -- see the design doc's
/// "Stack alignment" section for the derivation. A pure function, not a
/// method, so emit_prologue and emit_epilogue each independently compute
/// the identical value from the identical inputs and can never disagree.
///
/// PRECONDITION: `requested` must be a realistic spill-slot frame size
/// (nowhere near `u32::MAX`) -- plain `u32` addition here is unchecked,
/// so a `requested` within 16 bytes of `u32::MAX` could overflow/wrap.
/// Not a concern for any real JIT'd expression's spill footprint (which
/// would stack-overflow long before approaching 4GB), so this is
/// documented as a precondition rather than defended with checked
/// arithmetic against an unreachable input.
fn padded_spill_bytes(num_callee_saved: usize, requested: u32) -> u32 {
    let base_offset = (num_callee_saved as u32) * 8;
    let misalignment = (base_offset + requested) % 16;
    if misalignment == 0 {
        requested
    } else {
        requested + (16 - misalignment)
    }
}

/// Emits `push rbp; mov rbp, rsp; <push each callee_saved reg>; [sub rsp, N]`.
/// `callee_saved` must not contain Rbp (see module doc). `spill_bytes` is
/// the RAW requested spill-slot size -- this function pads it internally
/// for 16-byte alignment; callers should NOT pre-pad it themselves.
pub fn emit_prologue(asm: &mut Assembler, callee_saved: &[PhysReg], spill_bytes: u32) {
    assert!(
        !callee_saved.contains(&PhysReg::Rbp),
        "Rbp must not appear in callee_saved -- its save/restore is \
         handled unconditionally by emit_prologue/emit_epilogue themselves"
    );
    asm.push_reg(PhysReg::Rbp);
    asm.mov_reg_reg(PhysReg::Rbp, PhysReg::Rsp);
    for &reg in callee_saved {
        asm.push_reg(reg);
    }
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    if n > 0 {
        asm.alu_reg_imm(AluOp::Sub, PhysReg::Rsp, n as i32);
    }
}

/// Emits `[add rsp, N]; <pop each callee_saved reg, REVERSE order>; pop rbp; ret`.
/// Must be called with the EXACT SAME `callee_saved`/`spill_bytes` as the
/// matching emit_prologue call -- both independently compute the same
/// padded N via padded_spill_bytes, so as long as the inputs match, the
/// two are guaranteed symmetric.
pub fn emit_epilogue(asm: &mut Assembler, callee_saved: &[PhysReg], spill_bytes: u32) {
    assert!(
        !callee_saved.contains(&PhysReg::Rbp),
        "Rbp must not appear in callee_saved -- its save/restore is \
         handled unconditionally by emit_prologue/emit_epilogue themselves"
    );
    let n = padded_spill_bytes(callee_saved.len(), spill_bytes);
    if n > 0 {
        asm.alu_reg_imm(AluOp::Add, PhysReg::Rsp, n as i32);
    }
    for &reg in callee_saved.iter().rev() {
        asm.pop_reg(reg);
    }
    asm.pop_reg(PhysReg::Rbp);
    asm.ret();
}
```

All five `Assembler` methods used (`push_reg`, `pop_reg`, `mov_reg_reg`, `alu_reg_imm`, `ret`) already exist (Phase 6a-6f) — this slice calls them, doesn't add to `assembler.rs`.

## Testing

Golden-byte + `iced-x86` round-trip tests, matching Phase 6's discipline exactly (this is encoder-layer work, not `MachineInst`-layer work, so `machine_inst`'s golden-`Vec<MachineInst>` test style doesn't apply here — `round_trip.rs`'s style does):
- `emit_prologue`/`emit_epilogue` with empty `callee_saved` and `spill_bytes: 0` — the degenerate case, exact bytes `[push rbp, mov rbp/rsp]` and `[pop rbp, ret]` respectively, no `sub`/`add rsp` at all.
- With `spill_bytes` already a multiple of 16 and empty `callee_saved` — confirms `padded_spill_bytes` doesn't over-pad when no padding is needed.
- With `spill_bytes` NOT a multiple of 16 — confirms the padding arithmetic, with a hand-derived expected byte sequence (formulaic, not a magic hardcoded value, per this project's established `jmp`/`jcc` fixup-offset test style).
- With an ODD number of callee-saved registers (e.g. 1 or 3) and `spill_bytes: 0` — confirms padding correctly kicks in even when the caller requested zero spill bytes, purely because the callee-saved count itself creates misalignment.
- With an EVEN number of callee-saved registers and `spill_bytes: 0` — confirms NO padding/no `sub rsp` is emitted (both counts happen to already be 16-aligned).
- A full round trip: `emit_prologue` then some filler `Assembler` calls then `emit_epilogue` with the SAME `callee_saved`/`spill_bytes`, decoded via `iced-x86` and manually checked for symmetry (every pushed register gets popped in the right order).
- The `assert!` panics when `Rbp` is included in `callee_saved`, for both `emit_prologue` and `emit_epilogue` (`#[should_panic]`).
- `SYSV_CALLEE_SAVED`'s contents match SPEC.md's documented set minus `Rbp` (`Rbx`, `R12`-`R15`).

## Exit criteria

1. `emit_prologue`/`emit_epilogue` exist in `crates/forge-x64/src/prologue.rs`, exported from `lib.rs`.
2. Both correctly save/restore an arbitrary `callee_saved: &[PhysReg]` list (excluding `Rbp`) via real `push_reg`/`pop_reg` calls, in matching forward/reverse order.
3. `padded_spill_bytes` correctly pads `spill_bytes` for 16-byte total-frame alignment, accounting for `callee_saved`'s length; both emit functions use it identically.
4. Both functions `assert!` (not `debug_assert!`) if `Rbp` appears in `callee_saved`.
5. `SYSV_CALLEE_SAVED` constant exists with the correct 5-register set (no `Rbp`).
6. Tests cover the degenerate case, already-aligned spill sizes, misaligned spill sizes needing padding, odd/even callee-saved counts, a full prologue/epilogue round trip, and both panic cases.
7. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
8. No regressions in any Phase 6/7a/7b/7c `forge-x64` test or any other crate's tests.
