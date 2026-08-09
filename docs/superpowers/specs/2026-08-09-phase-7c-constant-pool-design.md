# Design: forge Phase 7c — Constant Pool & Sign-Mask Constants

**Status:** Approved for planning
**Scope:** The third sub-slice of CHECKLIST.md Phase 7 — two bullets: "Constant pool: f64 constants placed after the code, loaded RIP-relative" and "Sign-mask constants for `abs`/`neg`." Builds the constant-pool *data structure* (deduplicating interning of 8-byte constants) and changes `MachineInst::LoadImmF64`/`FloatAbs`/`FloatNeg` to reference pool entries instead of materializing values via a GPR round-trip.
**Out of scope (deferred):** Actual RIP-relative *byte emission* (calling 6f's `lea_reg_riprel`/`movsd_reg_riprel`, laying out the pool's bytes after the code, patching real offsets) — this is fundamentally a byte-emission-time concern, needing a real `Assembler` and real `PhysReg` assignments, neither of which exist at `MachineInst`-selection time. It belongs to the same post-Phase-8 "final wiring" step already established by 7a's design doc for resolving two-address copies and `Abs`/`Neg`'s (previous) GPR-materialization sequence. This slice builds the data this wiring step will consume — the pool itself, and `MachineInst`s that reference it — not the wiring step.

## Why this changes existing `MachineInst` shapes, not just adds new ones

7a's own design doc anticipated this exact evolution — its `FloatAbs`/`FloatNeg` doc comment says the sign mask is materialized "via `mov_reg_imm` + `movq_gpr_to_xmm`, **or eventually a RIP-relative constant pool**." This slice is that "eventually." Three `MachineInst` variants change shape:

1. **`LoadImmF64 { dst, bits: u64 }` → `LoadImmF64 { dst, pool_index: PoolIndex }`.** Every f64 literal (`Inst::ConstF64`) now interns its bit pattern into a shared `ConstantPool` instead of embedding the raw bits directly in the instruction. This is a real, free optimization, not just a representational change: two occurrences of the same f64 literal in one function (a common case — e.g. `0.5` appearing twice) now intern to the *same* pool slot, whereas before each got its own `LoadImmF64` with independently-embedded bits.
2. **`FloatAbs { dst, src, mask_tmp: Value }` → `FloatAbs { dst, src, mask_pool: PoolIndex }`** (same shape change for `FloatNeg`). The sign masks are fixed, program-wide constants (`0x7FFF_FFFF_FFFF_FFFF` for abs, `i64::MIN` for neg) — every `Abs`/`Neg` in the *entire function* now shares ONE pool entry per mask, rather than each minting its own synthetic `Value` and its own `LoadImmI64`. This also *removes* code: the `self.fresh(Ty::I64)` + `LoadImmI64` push sequence disappears entirely from these two arms, replaced by a single `self.pool.intern(mask_bits)` call.

`Fma`'s `mul_tmp: Value` synthetic-value mechanism is **unrelated and unchanged** — `mul_tmp` holds a *computed* intermediate result (the product `a*b`), not a compile-time constant, so it has nothing to intern into a pool and keeps using `Selector::fresh`/`synthetic_types` exactly as before.

## Architecture

New types in `crates/forge-x64/src/machine_inst/mod.rs` (no new files — the module is small enough after 7b's test-file split that this doesn't warrant one yet):

```rust
/// An index into a ConstantPool's entries. Opaque outside this module,
/// same "not usable across different pools" caveat as Label is for a
/// different Assembler (Phase 6a's precedent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PoolIndex(usize);

/// Deduplicating storage for the 8-byte constants (f64 literals, sign
/// masks) instruction selection needs a real memory location for.
/// Actual byte layout / RIP-relative addressing happens later, once a
/// real Assembler and PhysReg assignments exist (post-Phase-8) -- this
/// is purely the "what constants exist, and which ones are the same
/// value" bookkeeping.
#[derive(Default)]
pub struct ConstantPool {
    entries: Vec<u64>,
    index_of: HashMap<u64, PoolIndex>,
}

impl ConstantPool {
    /// Returns the existing PoolIndex if `bits` was already interned,
    /// otherwise appends a new entry and returns its index. This is the
    /// ONLY way constants enter the pool -- callers never construct a
    /// PoolIndex directly.
    pub fn intern(&mut self, bits: u64) -> PoolIndex {
        if let Some(&idx) = self.index_of.get(&bits) {
            return idx;
        }
        let idx = PoolIndex(self.entries.len());
        self.entries.push(bits);
        self.index_of.insert(bits, idx);
        idx
    }

    pub fn entries(&self) -> &[u64] {
        &self.entries
    }
}
```

`Selector` gains a `pool: ConstantPool` field (alongside `fully_fusable_scaled_indices` etc.), and `SelectedFunction` gains `pub pool: ConstantPool`, populated from `sel.pool` at the end of `select()` — the same "extend the struct, extend the final assembly in `select()`'s return" pattern 7b's `coalescing_hints` already established.

Two `select_inst` arms change:

```rust
// Inst::ConstF64's arm, changed from LoadImmF64{dst,bits:*bits} to:
Inst::ConstF64(bits) => {
    let pool_index = self.pool.intern(*bits);
    self.insts.push(MachineInst::LoadImmF64 { dst, pool_index });
}
```

```rust
// Inst::Abs's arm, changed to remove the fresh()/LoadImmI64 sequence:
Inst::Abs(a) => {
    let mask_pool = self.pool.intern(0x7FFF_FFFF_FFFF_FFFFu64);
    self.insts.push(MachineInst::FloatAbs { dst, src: *a, mask_pool });
}
```

```rust
// Inst::Neg's F64 branch, same change:
Ty::F64 => {
    let mask_pool = self.pool.intern(i64::MIN as u64);
    self.insts.push(MachineInst::FloatNeg { dst, src: *a, mask_pool });
}
```

Note the mask constants are interned as `u64` bit patterns (`i64::MIN as u64`, not `i64::MIN` directly) — `ConstantPool` stores raw 8-byte patterns uniformly regardless of whether they originated as an f64 bit pattern or an i64 bit pattern used as a bitmask; the distinction only matters to whatever eventually loads them (a `movsd`-shaped load for f64 constants vs. a `movq`-into-GPR-then-`andpd`/`xorpd` shaped load for masks — still the emission step's job, unchanged from 7a's original design intent, just now sourced from the pool instead of a synthetic per-call-site immediate).

## Testing

Golden-`SelectedFunction` tests (checking both `insts` and the new `pool`):
- `LoadImmF64`: one test confirming a single f64 literal interns to `PoolIndex(0)`.
- **Deduplication, the main new behavior this slice adds**: a function with the SAME f64 literal appearing twice (e.g. `0.5 + 0.5`, two separate `Inst::ConstF64` nodes with identical bits — since forge-opt's GVN might already merge these in optimized IR, construct this directly via the builder to test the selector's OWN deduplication independent of whether GVN ran) asserts both `LoadImmF64`s reference the SAME `PoolIndex`, and `pool.entries().len() == 1`.
- A function with two DIFFERENT f64 literals asserts two DIFFERENT `PoolIndex`es and `pool.entries().len() == 2`.
- `Abs`: confirms `FloatAbs`'s `mask_pool` references a pool entry with value `0x7FFF_FFFF_FFFF_FFFF`, and confirms NO synthetic `Value` is minted for it anymore (no entry in `synthetic_types` matching the old mask-temp pattern — this slice removes that mechanism for this specific case).
- `Neg` (float): same shape, confirms `mask_pool` references `i64::MIN as u64`.
- **Two `Abs` calls in one function share the mask pool entry**: `abs(x) + abs(y)` asserts both `FloatAbs`s reference the SAME `PoolIndex`, and `pool.entries().len() == 1` (just the one mask, even with two `Abs` call sites).
- **`Abs` and `Neg` do NOT share a pool entry with each other** (different masks): a function using both asserts `pool.entries().len() == 2` and the two `PoolIndex`es differ.
- Confirm `Fma`'s `mul_tmp` mechanism is genuinely unchanged: one test rebuilding 7a's original `Fma` golden-sequence test unmodified, confirming it still passes byte-for-byte identically (proving this slice didn't accidentally touch unrelated code).

## Exit criteria

1. `ConstantPool`/`PoolIndex` exist; `intern` deduplicates identical `u64` bit patterns and is the only way to obtain a `PoolIndex`.
2. `SelectedFunction::pool: ConstantPool` exists, populated from the `Selector`'s pool at the end of `select()`.
3. `MachineInst::LoadImmF64` carries `pool_index: PoolIndex` instead of `bits: u64`; `Inst::ConstF64`'s arm interns instead of embedding.
4. `MachineInst::FloatAbs`/`FloatNeg` carry `mask_pool: PoolIndex` instead of `mask_tmp: Value`; their arms intern the fixed mask constant instead of minting a synthetic `Value` + `LoadImmI64`.
5. Every existing 7a/7b test referencing the old `LoadImmF64`/`FloatAbs`/`FloatNeg` shapes is updated to the new shape (compile errors will force this — Rust's exhaustive field requirements make silently missing one impossible).
6. Tests cover single-constant interning, cross-call-site deduplication (both for f64 literals and for shared masks), non-deduplication of genuinely different constants, and confirm `Fma`'s unrelated `mul_tmp` mechanism is untouched.
7. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
8. No regressions in any Phase 6 `forge-x64` test or any other crate's tests.
