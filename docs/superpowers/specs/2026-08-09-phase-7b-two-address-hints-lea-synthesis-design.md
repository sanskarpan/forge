# Design: forge Phase 7b — Two-Address Coalescing Hints & `lea` Synthesis

**Status:** Approved for planning
**Scope:** The second sub-slice of CHECKLIST.md Phase 7 — two of its bullets: "Two-address fixup" (as coalescing-hint *generation*, not copy insertion — copy insertion is a post-Phase-8 emission-time decision, per 7a's design) and "`lea` synthesis for `a + b*k + c`" (recognizing an `Add(Mul(b, k), c)`-shaped IR tree, `k ∈ {2,4,8}`, and selecting a single non-destructive `lea` instead of two destructive `IntMul`+`IntAdd` instructions).
**Out of scope (deferred):** "Addressing-mode folding: `Load{base, offset}` folds into the memory operand of the consuming instruction" — `forge_ir::Inst` has no `Load`/`Store` variant (confirmed in 7a's research); this bullet describes an IR construct that doesn't exist in this language (no arrays/pointers/memory), so there is nothing to fold today. It stays open on CHECKLIST.md with a note explaining why, to be revisited if/when the language grows memory operations. "`Select` → `cmov`/blend" is **also explicitly deferred**, to its own future slice — see "Why Select→cmov is deferred" below. Full DAG-sharing-aware fusion (checking whether a fused `Mul`'s result is *also* used elsewhere and skipping its redundant standalone computation) is deferred too — see "Lea synthesis is redundancy-safe, not redundancy-free" below.

## Why `Select`→`cmov` is deferred

Unlike the other Phase 7 bullets, `Select`→`cmov` is a genuine optimization, not a correctness requirement: `if`/`else` already lowers correctly today via `Branch`+two blocks+`Phi` (7a's `Branch`/`Jump` lowering, plus Phase 8's planned SSA-deconstruction handling of `Phi`). Fusing a diamond CFG shape into a branchless `cmov`/register-round-trip sequence would only ever change performance, never correctness. It's also the one piece of this bullet-group needing a **fundamentally different mechanism** than everything else built so far: every prior lowering (7a's whole selector, and this slice's hint/lea work) operates strictly within `select_inst`'s per-`Value` dispatch; diamond fusion instead needs to recognize a **multi-block CFG shape** *before* the main per-block walk and skip/merge three blocks' worth of normal lowering into one instruction — a real architectural addition, not an incremental match-arm. Bundling it into this slice would risk destabilizing 7a's clean, already-tested per-instruction model for a feature with no way to even be benchmarked yet (there's no working end-to-end pipeline until Phase 8 exists). Deferred to a future slice once Phase 8 exists and diamond-fusion's actual performance value can be measured, not just assumed.

## Architecture

Both pieces live in `crates/forge-x64/src/machine_inst.rs`, extending `Selector`/`SelectedFunction` from 7a — no new files, no new crate dependencies.

**Coalescing hints** are a new `SelectedFunction::coalescing_hints: HashMap<Value, Value>` field (`dst -> preferred-same-location-as`), populated by a new pass `compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value>` that runs once, after `select_inst`/`select_term` have produced the full `Vec<MachineInst>`. For every 2-address-destructive `MachineInst` variant (binary: `IntAdd`/`IntSub`/`IntMul`/`And`/`Or`/`Xor`/`Shl`/`Shr`/`Sar`/`FloatAdd`/`FloatSub`/`FloatMul`/`FloatDiv`/`FloatMin`/`FloatMax`, where the real x86 instruction computes `dst = dst OP rhs` and so wants `dst`'s register to already hold `lhs`'s value; unary: `IntNeg`/`Not`/`FloatNeg`/`FloatAbs`, where `dst` wants to already hold `src`'s value), record `dst -> lhs` (or `dst -> src`). `IntDiv`/`IntRem` are excluded — their real hardware constraint is fixed `RAX`/`RDX` placement, not "same register as an operand," a different kind of hint Phase 8 will need to handle as a *fixed-register* constraint (already anticipated by CHECKLIST's Phase 8 `Interval.fixed` field), not a coalescing one. This is purely a lookup table — it doesn't change `insts` at all, matching 7a's principle that `MachineInst` stays 3-address SSA form throughout Phase 7.

**`lea` synthesis** extends `select_inst`'s existing `Inst::Add` arm (7a's `Ty`-dispatching match). Before falling through to the existing `IntAdd`/`FloatAdd` dispatch, the `I64` case is checked against two tree shapes: `Add(Mul(b, ConstI64(k)), c)` and `Add(c, Mul(b, ConstI64(k)))`, for `k ∈ {2, 4, 8}` (`lea`'s only legal SIB scale values — `k=1` is deliberately excluded, since scale=1 buys nothing over a plain `IntAdd` and isn't worth the special-casing). Recognizing the shape means looking at an *operand's defining instruction* (`self.func.insts[operand.0 as usize]`) for the first time in this selector — 7a's arms only ever looked at an operand's *type* (`ty_of`), never its *defining instruction*. When the shape matches, `select_inst` emits `MachineInst::Lea { dst, base: c, index: b, scale: k, disp: 0 }` instead of `IntAdd`.

## Lea synthesis is redundancy-safe, not redundancy-free

This slice does **not** implement DAG-sharing-aware fusion. If `Mul(b, k)`'s result `Value` is *also* used somewhere else in the function (not just by this one `Add`), that `Mul` still gets its own ordinary, independent `IntMul` selected when the RPO walk reaches its own defining position — `lea`-fusing it into this `Add` does not suppress or delete that. This means the multiply may be computed twice (once standalone, once folded into the `lea`'s address computation) when its result is shared. This is a deliberate, explicit scope simplification: implementing full sharing-awareness would require a whole-function use-count pass (`forge_ir::uses_of` already exists and could build one, but wiring that up, correctly handling the selector's own synthetic values, and suppressing the redundant `IntMul` selection when it's later visited in the walk is real additional machinery) for a case that's rare in practice (a strength-reduction-shaped multiply-by-constant being reused elsewhere unchanged) and never a *correctness* concern — only a missed-optimization one. Always-fuse-when-the-shape-matches is correct in every case; it just isn't always maximally efficient. A future slice can add sharing-awareness if profiling ever shows it matters.

## Components

### `SelectedFunction::coalescing_hints` and `compute_coalescing_hints`

```rust
pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    /// dst -> the Value dst should end up sharing a physical register/slot
    /// with, if Phase 8's allocator can manage it. Every entry corresponds
    /// to a 2-address-destructive x86 operation (see compute_coalescing_hints)
    /// where honoring the hint lets the final MachineInst-to-bytes emission
    /// step skip an otherwise-mandatory `mov dst, lhs` copy. A hint that
    /// isn't honored is not an error -- emission falls back to inserting
    /// the copy.
    pub coalescing_hints: HashMap<Value, Value>,
}

/// Scans a fully-selected instruction sequence and records a dst->operand
/// coalescing hint for every 2-address-destructive MachineInst. Binary ops
/// hint dst->lhs (the operand whose register `dst` needs to already hold);
/// unary ops hint dst->src. IntDiv/IntRem are deliberately excluded -- their
/// constraint is fixed RAX/RDX placement, a different (fixed-register, not
/// coalescing) hint Phase 8's allocator handles separately.
pub fn compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value> {
    let mut hints = HashMap::new();
    for inst in insts {
        match inst {
            MachineInst::IntAdd { dst, lhs, .. }
            | MachineInst::IntSub { dst, lhs, .. }
            | MachineInst::IntMul { dst, lhs, .. }
            | MachineInst::And { dst, lhs, .. }
            | MachineInst::Or { dst, lhs, .. }
            | MachineInst::Xor { dst, lhs, .. }
            | MachineInst::Shl { dst, lhs, .. }
            | MachineInst::Shr { dst, lhs, .. }
            | MachineInst::Sar { dst, lhs, .. }
            | MachineInst::FloatAdd { dst, lhs, .. }
            | MachineInst::FloatSub { dst, lhs, .. }
            | MachineInst::FloatMul { dst, lhs, .. }
            | MachineInst::FloatDiv { dst, lhs, .. }
            | MachineInst::FloatMin { dst, lhs, .. }
            | MachineInst::FloatMax { dst, lhs, .. } => {
                hints.insert(*dst, *lhs);
            }
            MachineInst::IntNeg { dst, src }
            | MachineInst::Not { dst, src }
            | MachineInst::FloatNeg { dst, src, .. }
            | MachineInst::FloatAbs { dst, src, .. } => {
                hints.insert(*dst, *src);
            }
            _ => {}
        }
    }
    hints
}
```

`select(func)` (7a's entry point) calls this once at the end, populating `SelectedFunction::coalescing_hints`.

### `MachineInst::Lea` and `lea` synthesis in `select_inst`

```rust
// New MachineInst variant, added near the integer-arithmetic group
Lea { dst: Value, base: Value, index: Value, scale: u8, disp: i32 },
```

```rust
// select_inst's Inst::Add arm, extended -- the F64/Bool cases are
// unchanged from 7a; only the I64 case gains a shape check first.
Inst::Add(a, b) => match self.ty_of(*a) {
    Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
    Ty::I64 => {
        if let Some(lea) = self.try_synthesize_lea(dst, *a, *b) {
            self.insts.push(lea);
        } else {
            self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b });
        }
    }
    Ty::Bool => unreachable!("Add never applies to Bool"),
},
```

```rust
impl<'a> Selector<'a> {
    /// Recognizes `Add(Mul(b, k), c)` or `Add(c, Mul(b, k))` for k in
    /// {2,4,8} and returns the equivalent Lea, or None if neither operand
    /// is a real IR value defined by such a Mul (synthetic values, which
    /// never appear as an Add's operand in this slice, are never checked
    /// here -- only real func.insts entries can be).
    fn try_synthesize_lea(&self, dst: Value, a: Value, b: Value) -> Option<MachineInst> {
        if let Some((base, index, scale)) = self.match_mul_by_pow2(a, b) {
            return Some(MachineInst::Lea { dst, base, index, scale, disp: 0 });
        }
        if let Some((base, index, scale)) = self.match_mul_by_pow2(b, a) {
            return Some(MachineInst::Lea { dst, base, index, scale, disp: 0 });
        }
        None
    }

    /// Checks whether `mul_candidate` is a real IR value defined by
    /// `Mul(index, ConstI64(k))` or `Mul(ConstI64(k), index)` for k in
    /// {2,4,8}; if so, returns (base, index, k) with `base` set to the
    /// OTHER argument passed in (`other`).
    fn match_mul_by_pow2(&self, mul_candidate: Value, other: Value) -> Option<(Value, Value, u8)> {
        if (mul_candidate.0 as usize) >= self.func.insts.len() {
            return None; // synthetic value, never Mul-defined
        }
        let defining = &self.func.insts[mul_candidate.0 as usize];
        let (m_a, m_b) = match defining {
            Inst::Mul(x, y) => (*x, *y),
            _ => return None,
        };
        let scale_from = |v: Value| -> Option<(Value, u8)> {
            if (v.0 as usize) >= self.func.insts.len() {
                return None;
            }
            match &self.func.insts[v.0 as usize] {
                Inst::ConstI64(k) if matches!(k, 2 | 4 | 8) => Some((v, *k as u8)),
                _ => None,
            }
        };
        if let Some((_, k)) = scale_from(m_b) {
            return Some((other, m_a, k));
        }
        if let Some((_, k)) = scale_from(m_a) {
            return Some((other, m_b, k));
        }
        None
    }
}
```

## Testing

Golden `Vec<MachineInst>` tests, same style as 7a:
- `compute_coalescing_hints`: one test covering a representative binary op (proves `dst -> lhs`, not `dst -> rhs`), one for a unary op, one confirming `IntDiv`/`IntRem` produce NO hint entry, one confirming a `MachineInst` with no natural hint (e.g. `Param`, `Jump`) is correctly absent from the map.
- `lea` synthesis: `Add(Mul(b,4), c)`, `Add(c, Mul(b,4))` (operand-order symmetry), a negative case where the multiplier isn't a power-of-2-in-{2,4,8} constant (e.g. `Mul(b, 3)` — must NOT synthesize a `Lea`, falls back to plain `IntMul`+`IntAdd`), a negative case where the "constant" operand is itself non-constant (e.g. `Mul(b, c)`, both real values — must not synthesize), and a redundancy-safety test proving that when the `Mul`'s result is ALSO used elsewhere (a second consumer), the `Mul` still gets independently selected as its own `IntMul` (documenting the accepted redundant-computation tradeoff, not hiding it).

## Exit criteria

1. `SelectedFunction::coalescing_hints` exists, populated by `compute_coalescing_hints`, covering every 2-address-destructive `MachineInst` variant, correctly excluding `IntDiv`/`IntRem`.
2. `MachineInst::Lea` exists; `select_inst`'s `Inst::Add`/`I64` case recognizes `Add(Mul(b,k),c)`/`Add(c,Mul(b,k))` for `k ∈ {2,4,8}` and emits it; falls back to plain `IntAdd` for every other shape.
3. Tests cover both operand orderings, the non-power-of-2 negative case, the non-constant-multiplier negative case, and the documented redundant-computation case.
4. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
5. No regressions in any Phase 6 `forge-x64` test, Phase 7a's `machine_inst` tests, or any other crate's tests.
6. CHECKLIST.md's "Two-address fixup," "Addressing-mode folding," and "`lea` synthesis" bullets are annotated to reflect what was actually built (or explicitly why not, for addressing-mode folding), and the "`Select`→`cmov`" bullet is annotated with the deferral reasoning above.
