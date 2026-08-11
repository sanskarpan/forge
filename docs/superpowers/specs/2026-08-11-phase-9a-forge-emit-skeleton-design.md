# Phase 9a: forge-emit Skeleton, Control Flow, Constant Pool, Happy-Path Translation

## Scope

Phase 9 ("Final Code Emission") lowers `MachineInst` (forge-x64, post instruction-selection) plus
`forge_regalloc::allocate()`'s output (`Location` per `Value`, spill-frame byte count) into real
x86-64 bytes via the existing `Assembler` (forge-x64, Phase 6), and wires the result into the
existing mmap JIT harness (`forge-mem`). It is decomposed into six sub-slices:

- **9a (this doc)** — new crate `forge-emit`; block/label layout; `Jump`/`Branch`/`Return` → real
  control flow; constant pool placement + RIP-relative loads; straightforward translation for the
  case where every `Value` involved is already in a register (`Location::Reg`, never
  `Location::Spill`).
- **9b** — Param ABI-mismatch copy, `IntDiv`/`IntRem` third-party rax/rdx clobber save/restore,
  `Shl`/`Shr`/`Sar` CL fixup with RCX displacement.
- **9c** — `Location::Spill` reload-before-use / store-after-def via `SCRATCH_GPR`/`SCRATCH_XMM`.
- **9d** — coalescing mov elision, parallel-copy insertion for unresolved phis.
- **9e** — `CallLibm` sequence: caller-saved XMM spill/restore, arg marshalling, return placement,
  call-site alignment.
- **9f** — prologue/epilogue wiring, top-level `emit_function` driver, forge-mem integration
  (including new int-typed `CompiledExpr` call paths), the full deferred integration-test corpus.

This document covers 9a only.

## Why a new crate

`forge-regalloc` already has a real (non-dev) dependency on `forge-x64` (for `PhysReg`). The
emission pipeline needs both `MachineInst` (forge-x64) and `Location`/`Interval`
(forge-regalloc) as real inputs, but `forge-x64` cannot add a real dependency on `forge-regalloc`
without creating a cycle. `forge-emit` is a new crate depending on both `forge-ir`, `forge-x64`,
and `forge-regalloc`, matching this project's existing one-crate-per-concern layout. It does not
depend on `forge-mem` yet — that wiring is 9f's job, once there's a complete `Vec<u8>` to hand it.

## Architecture

```
crates/forge-emit/
  Cargo.toml           # forge-ir, forge-x64, forge-regalloc deps
  src/
    lib.rs              # pub use of the module below
    translate.rs         # translate_inst(): MachineInst -> Assembler calls (this slice's core)
    layout.rs             # block layout: Label per Block, control-flow emission
    const_pool.rs          # constant pool placement + RIP-relative load emission
```

### `layout.rs` — block layout and control flow

`SelectedFunction::block_starts: Vec<(Block, usize)>` gives RPO-ordered block boundaries into
`insts`. 9a's driver (`emit_body`, see below) walks `block_starts`, and for each block:

1. Calls `asm.bind(label_for(block))` — every block gets a `Label` up front, allocated via
   `asm.new_label()` in a single pass over `block_starts` before any instruction is emitted (so
   forward references — a `Branch`'s `then_`/`else_`, a `Jump`'s `target` — are always resolvable
   at the point they're translated, per the existing `Assembler` label/fixup contract; no
   two-pass/backpatch logic of our own is needed, `Assembler` already does this).
2. Translates each `MachineInst` in the block's slice via `translate_inst` (see below).
3. Translates the block's terminator:
   - `MachineInst::Jump { target }` → `asm.jmp(label_for(target))`.
   - `MachineInst::Branch { cond, then_, else_ }` → `asm.test_reg_reg(cond_reg, cond_reg)` +
     `asm.jcc(ConditionCode::NotZero, label_for(then_))` + `asm.jmp(label_for(else_))`. (`cond`'s
     register is read from the assignment map like any other operand — see "Operand resolution"
     below. No fallthrough elision in this slice: even if `else_` is the next block in RPO order,
     we still emit the unconditional `jmp`. Fallthrough elision is a real, later optimization
     opportunity, explicitly out of scope here — see "Out of scope" below.)
   - `MachineInst::Return { value }` — 9a emits only the value-placement half (move `value`'s
     register into the ABI return register: `rax` for `I64`/`Bool`, `xmm0` for `F64` — determined
     from `SelectedFunction::synthetic_types`/`func.types` the same way `find_fusable_diamonds`
     already looks up a `Value`'s `Ty`). The actual `ret` instruction is emitted by the epilogue,
     which is 9f's job — 9a's driver takes an `is_last_block: bool` per block and only the
     function's designated exit point (see below) triggers it; for 9a's own tests, which call
     `emit_body` directly rather than the not-yet-existing `emit_function`, a bare `asm.ret()` is
     appended immediately after the return-value move so the test corpus is independently runnable
     through forge-mem without waiting on 9f. This is a real, documented, temporary duplication:
     9f's driver will not call `emit_body` as an untouched black box for the return case — it
     splices the epilogue in between the value-placement move and `ret()`. 9a's own tests own their
     `ret()` emission directly; they do not call a "the-real-driver" function that doesn't exist
     yet.

A `Function` can have multiple blocks ending in `Return` only through unreachable-code paths (SSA
construction only ever produces one live return point per the front end today — confirmed by
`forge_ir::builder::Builder`'s construction, which emits exactly one `Terminator::Return` for the
whole function) — 9a's tests do not need to special-case multiple returns, but `translate_inst`'s
`Return` handling itself is not position-sensitive (it just moves a value and returns), so it would
be correct even if that assumption changes later.

### `const_pool.rs` — constant pool placement

`ConstantPool::entries() -> &[u64]` gives the deduplicated raw bit patterns to place. Placement
happens once, after all code bytes are emitted (mirroring Phase 6f's own `lea_reg_riprel`/
`movsd_reg_riprel` design, which already anticipated "a real constant-pool system... placed after
the code"):

```rust
pub fn place_pool(asm: &mut Assembler, pool: &ConstantPool) -> Vec<Label> {
    pool.entries()
        .iter()
        .map(|&bits| {
            let label = asm.new_label();
            asm.bind(label);
            asm.emit_u64(bits); // raw little-endian bytes, no instruction encoding
            label
        })
        .collect()
}
```

This requires one new `Assembler` primitive, `emit_u64(&mut self, bits: u64)`, appending 8 raw
bytes to `code` with no instruction encoding around them (not a new addressing mode — just a raw
data emission helper, the same category of primitive as `code()` itself). This is the one small
addition to `forge-x64::Assembler` this slice needs; everything else it uses already exists.

`MachineInst::LoadImmF64 { dst, pool_index }` translates to
`asm.movsd_reg_riprel(dst_reg, pool_labels[pool_index])`.
`MachineInst::FloatAbs`/`FloatNeg`'s `mask_pool: PoolIndex` translates to
`asm.lea_reg_riprel(scratch_reg, pool_labels[mask_pool])` (loading the mask's *address* into a
GPR) followed by `asm.andpd_reg_reg`/`xorpd_reg_reg` against a value loaded from that address —
concretely, since `andpd`/`xorpd` only have reg-reg forms in the current `Assembler` (confirmed:
`andpd_reg_reg`, `xorpd_reg_reg`, no `_mem` variant), the mask is first loaded into an XMM scratch
register via `movsd_reg_riprel` (reusing the same load path as `LoadImmF64`), then
`andpd_reg_reg`/`xorpd_reg_reg` against that. No `lea_reg_riprel`-into-GPR-then-dereference path is
needed after all — `movsd_reg_riprel` already loads the 8 mask bytes directly into an XMM register
by address, which is exactly the operand `andpd`/`xorpd` need. The scratch register for this load
is `Xmm14` (`SCRATCH_XMM[0]`, from `forge_regalloc::SCRATCH_XMM`) — 9a takes a dependency on this
constant even though full spill-reload scratch usage is 9c's concern, because this specific need
(a transient register to hold the loaded mask, never live across any other instruction) exists
independently of spilling and is a permanent feature of `FloatAbs`/`FloatNeg`'s lowering, not a 9c
stopgap.

### `translate.rs` — per-instruction translation

Signature: `fn translate_inst(asm: &mut Assembler, inst: &MachineInst, loc: &impl Fn(Value) -> PhysReg, pool_labels: &[Label])`.

`loc: &impl Fn(Value) -> PhysReg` is 9a's operand-resolution interface: given a `Value`, return the
`PhysReg` holding it. In 9a, its only real implementation is
`|v| match assignment[&v] { Location::Reg(r) => r, Location::Spill(_) => panic!("not yet: spilled operand (Phase 9c)") }`
— i.e., 9a's own driver constructs this closure from the `HashMap<Value, Location>` `allocate()`
produced, and the `Location::Spill` arm is where 9a's documented scope boundary actually lives in
code, not just in prose. Every 9a test constructs interval/assignment data (directly, not via a
real `allocate()` call sized to force spilling) such that this arm is never hit.

**Full match-arm-by-match-arm treatment for every `MachineInst` variant** (all variants get a real
arm; this match is exhaustive from the start — no wildcard, no `todo!()` catch-all — matching this
codebase's established "new variant must force every call site to make an explicit choice"
discipline used throughout `select_inst`/`reads_of`/`def_of`):

| Variant | 9a translation | Deferred to |
|---|---|---|
| `LoadImmI64{dst,imm}` | `mov_reg_imm(dst_reg, imm)` | — (fully handled) |
| `LoadImmF64{dst,pool_index}` | `movsd_reg_riprel(dst_reg, pool_labels[pool_index])` | — |
| `IntAdd/Sub/And/Or/Xor{dst,lhs,rhs}` | 2-addr fixup: if `dst_reg != lhs_reg`, `mov_reg_reg(dst_reg,lhs_reg)` first (this is exactly what `coalescing_hints` is *for* — 9a does NOT yet consult `coalescing_hints` to skip this mov even when the allocator happened to already coincide `dst`/`lhs`; that elision is 9d's job — 9a always emits the guard-checked mov, correctness first); then `alu_reg_reg(op, dst_reg, rhs_reg)` | mov-elision → 9d |
| `IntMul{dst,lhs,rhs}` | same 2-addr fixup, then `imul_reg_reg(dst_reg, rhs_reg)` | mov-elision → 9d |
| `IntDiv{dst,lhs,rhs}` | if `lhs_reg != Rax`, `mov_reg_reg(Rax, lhs_reg)`; `cqo()`; `idiv_reg(rhs_reg)`; if `dst_reg != Rax`, `mov_reg_reg(dst_reg, Rax)` — **panics if this sequence would clobber a live value already resident in `Rax`/`Rdx` that is neither `lhs` nor `dst`** (9a cannot detect this itself without liveness data it doesn't consume — see "Panic policy" below) | third-party clobber → 9b |
| `IntRem{dst,lhs,rhs}` | identical, reading result from `Rdx` instead of `Rax` | third-party clobber → 9b |
| `IntNeg{dst,src}` | 2-addr fixup then `neg_reg(dst_reg)` | mov-elision → 9d |
| `Not{dst,src}` | 2-addr fixup then `not_reg(dst_reg)` | mov-elision → 9d |
| `Shl/Shr/Sar{dst,lhs,rhs}` | 2-addr fixup (`dst`←`lhs`) then: if `rhs_reg == Cl`, `shift_reg_cl(op, dst_reg)` directly; **else panics** — placing an arbitrary `rhs_reg` into `Cl` when something else already lives in `Rcx` is exactly 9b's displace/restore job | CL fixup → 9b |
| `Lea{dst,base,index,scale,disp}` | `lea_reg_scaled(dst_reg, base_reg, index_reg, scale, disp)` | — |
| `IntCmov{dst,cond,then_val,else_val}` | 2-addr fixup (`dst`←`then_val`) then `test_reg_reg(cond_reg,cond_reg)` + `cmovcc(ConditionCode::Zero, dst_reg, else_val_reg)` (per the design doc committed with Phase 7f: CMOVZ picks `else_val` iff `cond==0`) | mov-elision → 9d |
| `FloatAdd/Sub/Mul/Div/Min/Max{dst,lhs,rhs}` | 2-addr fixup (`movsd_reg_reg` if `dst_reg != lhs_reg`) then `sse_reg_reg(op, dst_reg, rhs_reg)` | mov-elision → 9d |
| `FloatSqrt{dst,src}` | 2-addr fixup then `sse_reg_reg(SseOp::Sqrt, dst_reg, dst_reg)` (single-operand form: `Sqrt` reads and writes the same register per its existing golden-byte tests) | — |
| `FloatRound{dst,src,mode}` | `roundsd(mode, dst_reg, src_reg)` (no 2-addr fixup needed — `roundsd`'s existing encoding takes independent dst/src operands, confirmed against its Phase 6 golden bytes) | — |
| `FloatAbs/FloatNeg{dst,src,mask_pool}` | 2-addr fixup (`dst`←`src`) then load mask into `Xmm14` via `movsd_reg_riprel`, then `andpd_reg_reg`(Abs)/`xorpd_reg_reg`(Neg) `(dst_reg, Xmm14)` | mov-elision → 9d |
| `IntCmp{op,dst,lhs,rhs}` | `alu_reg_reg(AluOp::Cmp, lhs_reg, rhs_reg)` (no 2-addr fixup — `cmp` doesn't write `lhs`) then `xor_reg_reg(dst_reg,dst_reg)` **before** the compare (zero-extension, ordering matters: `xor` must not clobber flags the subsequent `setcc` reads — `xor r,r` does set flags, but `setcc` reads different flags than `xor` sets in a way that would matter here: **actually this ordering is wrong, corrected below**) | — |
| `FloatCmp{op,dst,lhs,rhs}` | `ucomisd_reg_reg(lhs_reg, rhs_reg)` then zero-extend + `setcc` (same corrected pattern as `IntCmp`, unsigned condition codes) | — |
| `IntToFloat{dst,src}` | `cvtsi2sd(dst_reg, src_reg)` | — |
| `FloatToInt{dst,src}` | `cvttsd2si(dst_reg, src_reg)` | — |
| `CallLibm{..}` | **panics unconditionally** — no arm produces any bytes; this is 9e's entire job, nothing about it is "mostly simple" the way Param/IntDiv are | full sequence → 9e |
| `Jump/Branch/Return` | handled by `layout.rs`, not `translate_inst` (terminators, not body instructions — `SelectedFunction::insts` never contains them; confirmed by `select()`'s existing structure, which calls `select_term` separately from `select_inst`) | n/a |
| `Param{dst,index}` | **panics unconditionally** — 9a's tests only exercise zero-param functions (constant-only expressions) or hand-built `SelectedFunction`s that don't include `Param`; real ABI-register placement is 9b's job per the approved decomposition | ABI placement → 9b |

**Correction on `IntCmp`/`FloatCmp` zero-extension ordering** (caught while drafting the table
above — recorded here because it's exactly the kind of ordering bug this project's review process
exists to catch, and it's cheaper to fix during design than to let it reach implementation):
`setcc` writes only the low byte and leaves the upper 56 bits of `dst_reg` as whatever they were
before (confirmed in the Phase 6 research: "`setcc` ... writes **only the low byte**, upper bits
undefined"). Zeroing `dst_reg` must happen **before** the flags-setting compare, not after, and
must not itself be a flag-clobbering operation that survives to `setcc` incorrectly — but since
`setcc` is the *very next* instruction after the compare and reads only the compare's flags (not
the zeroing step's), the correct sequence is: `xor_reg_reg(dst_reg, dst_reg)` (zero it, this also
sets flags harmlessly since nothing after it depends on *these* flags) →
`alu_reg_reg(AluOp::Cmp, lhs_reg, rhs_reg)` (sets the real flags `setcc` will read) →
`setcc(cc, dst_reg)` (writes low byte only, upper bits already zero from the `xor`). One subtlety:
if `dst_reg == lhs_reg` or `dst_reg == rhs_reg` (i.e., the comparison's own destination happens to
coincide with one of its operands' registers — possible since `IntCmp`/`FloatCmp` don't
participate in `coalescing_hints`, so the allocator is free to assign them independently, but
"independently" doesn't forbid accidental coincidence), zeroing `dst_reg` first would destroy an
operand before the compare reads it. **Fix**: order is `cmp`/`ucomisd` (read `lhs`/`rhs`) **first**,
then `xor_reg_reg(dst_reg,dst_reg)`, then `setcc` — except `xor` clobbers flags, which would
destroy what `setcc` needs to read. **Real fix**: zero `dst_reg` via `mov_reg_imm(dst_reg, 0)`
instead of `xor_reg_reg` — `mov` doesn't touch flags — placed *before* the compare if `dst_reg`
doesn't alias `lhs`/`rhs`, or the compare can simply run first when it does, since `mov_reg_imm`
never touches flags either way and can safely run after the compare too. **Final resolved
sequence, order-independent and alias-safe**: `alu_reg_reg(AluOp::Cmp, lhs_reg, rhs_reg)` →
`mov_reg_imm(dst_reg, 0)` → `setcc(cc, dst_reg)`. Compare first (reads operands while they're
still whatever the allocator gave them, before `dst_reg` is touched at all), then zero (safe
regardless of aliasing, since the compare has already consumed `lhs`/`rhs`), then `setcc` (flags
from the compare are still valid — `mov_reg_imm` doesn't touch them).

### Panic policy for this slice

Every deferred-to-9b/9c/9d/9e case above panics with a message naming the specific gap and the
sub-slice that closes it (e.g. `"forge-emit (Phase 9a): Param placement not yet implemented — Phase 9b"`).
This is not a TODO or a silent no-op: it is a loud, immediate, descriptive failure on any input
this slice doesn't yet support, and every one of 9a's own tests is constructed to avoid triggering
it. `debug_assert!`-vs-`panic!` is not a live question here — these are `panic!`s unconditionally
(not gated to debug builds), since hitting one means the emitted code would otherwise be silently
wrong, not just slower to fail — the same "loud failure over silent corruption" principle already
applied throughout Phase 7f's `find_fusable_diamonds`.

## Testing

`crates/forge-emit/src/translate.rs` gets a `#[cfg(test)] mod tests` using the same hand-built
`Function`/`BlockData`/`push_inst` pattern established in `forge-x64`'s own test modules (since
`forge_ir::builder::Builder`'s phi machinery is private, and 9a's tests need precise control over
which `MachineInst`s appear). Each test: hand-build a small `SelectedFunction` (or call
`forge_x64::select()` on a hand-built `Function` where convenient), hand-build an
`assignment: HashMap<Value, Location>` using only `Location::Reg`, call `emit_body`, then execute
the resulting bytes through `forge_mem::ExecutableBuffer`/`CompiledExpr` and assert on the actual
returned `f64`/`i64` value — genuine execution tests, not byte-comparison golden tests (unlike
Phase 6's encoder tests, which golden-byte-test single instructions in isolation; 9a is assembling
multi-instruction sequences where the *behavior* is what matters, and byte-exact golden tests would
be brittle against equally-correct instruction orderings). This makes `forge-emit` depend on
`forge-mem` in `[dev-dependencies]` only (not `[dependencies]` — 9f is what adds the real
dependency once there's a top-level driver worth wiring end-to-end).

Test corpus for 9a (all constructed to avoid every panic arm above):
- A single `Return{ImmediateConstant}` — smallest possible runnable program.
- Straight-line arithmetic: `LoadImmF64` × 2 → `FloatAdd` → `Return` (constant pool round-trip).
- `LoadImmI64` × 2 → `IntAdd`/`IntSub`/`IntMul` → `Return`.
- `IntDiv`/`IntRem` on two `LoadImmI64`s where neither operand's register happens to alias a
  third live value (i.e., the "no third-party clobber" case this slice DOES handle) → `Return`.
- `Shl`/`Shr`/`Sar` where the shift-amount operand's assigned register is deliberately set to
  `Rcx` in the hand-built assignment (the case 9a DOES handle) → `Return`.
- `FloatAbs`/`FloatNeg` on a negative/positive `LoadImmF64` → `Return`, checking the sign bit
  actually flips via the returned value's sign.
- `IntCmp`/`FloatCmp` for each of the 6 `CmpOp` variants, checking the returned 0/1 (as an `I64`
  return) is correct, INCLUDING a case where the comparison's `dst` register is deliberately
  assigned to alias one of `lhs`/`rhs` in the hand-built assignment (exercises the
  aliasing-safety fix above — this is exactly the kind of case that would silently break under the
  wrong ordering, so it must be a real test, not just design-doc prose).
- `IntCmov` for both the then-taken and else-taken case (`cond` = 1 and 0 respectively).
- A `Branch` diamond (two blocks, one taken based on a runtime-computed condition, not a constant)
  → `Return`, confirming control flow and label resolution both directions.
- A `Jump`-only straight-line multi-block function (no branch), confirming block layout/label
  binding works even without conditional control flow.
- Three explicit panic tests (`#[should_panic(expected = "...")]`): `Param`, `CallLibm`, and a
  `Shl` whose shift-amount register is NOT `Rcx` in the hand-built assignment — confirming the
  documented scope boundary is real, not just prose (mirrors this project's established practice
  of testing that a deferred gate actually gates, e.g. Phase 7f's
  `f64_diamond_with_a_non_cmp_cond_is_gated_off_the_int_cmov_path` test).

## Exit criteria

1. `crates/forge-emit` exists, builds, depends on `forge-ir`/`forge-x64`/`forge-regalloc` (real)
   and `forge-mem` (dev-only).
2. `forge_x64::Assembler` gains exactly one new primitive, `emit_u64`, tested with its own
   golden-byte unit test (raw 8-byte little-endian emission, no encoding).
3. `translate_inst` has a real (not wildcard) arm for every `MachineInst` variant; every arm either
   fully implements 9a's documented scope or panics with a message naming the deferring sub-slice.
4. `place_pool`/constant-pool RIP-relative loading works for both `LoadImmF64` and
   `FloatAbs`/`FloatNeg`'s mask constants.
5. `Jump`/`Branch`/`Return` control flow resolves correctly via `Assembler`'s existing label/fixup
   mechanism, forward and backward.
6. Every test in the corpus above passes by actually executing the emitted bytes through
   `forge-mem` and checking the returned value — not by inspecting bytes.
7. The three `#[should_panic]` tests confirm the scope boundary is enforced in code.
8. `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo fmt --check` all clean.
9. CHECKLIST.md gets a note (following this project's established convention) recording what 9a
   built, what it explicitly deferred and to which later sub-slice, and pointing at this design doc.

## Out of scope (explicitly, for this slice)

- Fallthrough elision for `Branch`'s `else_` when it's the next block in RPO order (a real, later
  micro-optimization; not needed for correctness).
- Anything involving `Location::Spill` (9c), `coalescing_hints`-driven mov elision or phi
  resolution (9d), `CallLibm` (9e), the real top-level `emit_function` driver / prologue-epilogue
  splicing / forge-mem's non-dev integration (9f).
- Win64 ABI (already out of scope project-wide, per Phase 7d/7e's own notes).
