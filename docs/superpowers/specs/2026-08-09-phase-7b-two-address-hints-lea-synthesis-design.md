# Design: forge Phase 7b — Two-Address Coalescing Hints & `lea` Synthesis

**Status:** Approved for planning
**Scope:** The second sub-slice of CHECKLIST.md Phase 7 — two of its bullets: "Two-address fixup" (as coalescing-hint *generation*, not copy insertion — copy insertion is a post-Phase-8 emission-time decision, per 7a's design) and "`lea` synthesis for `a + b*k + c`" (recognizing an `Add(Mul(b, k), c)`-shaped IR tree, `k ∈ {2,4,8}`, and selecting a single non-destructive `lea` instead of two destructive `IntMul`+`IntAdd` instructions).
**Out of scope (deferred):** "Addressing-mode folding: `Load{base, offset}` folds into the memory operand of the consuming instruction" — `forge_ir::Inst` has no `Load`/`Store` variant at all (confirmed in 7a's research, and forge's language has no arrays/pointers/memory operations of any kind); this bullet describes an IR construct that doesn't exist in this language, so there is nothing to fold today. It stays open on CHECKLIST.md with a note explaining why, to be revisited if/when the language grows memory operations. "`Select` → `cmov`/blend" is **also explicitly deferred — to Phase 7f, a new named slice** (not implemented here) — see "Why Select→cmov is deferred" below.

## Why `Select`→`cmov` is deferred to Phase 7f

Unlike the other Phase 7 bullets, `Select`→`cmov` is a genuine optimization, not a correctness requirement: `if`/`else` already lowers correctly today via `Branch`+two blocks+`Phi` (7a's `Branch`/`Jump` lowering, plus Phase 8's planned SSA-deconstruction handling of `Phi`). Fusing a diamond CFG shape into a branchless `cmov`/register-round-trip sequence would only ever change performance, never correctness. It's also the one piece of this bullet-group needing a **fundamentally different mechanism** than everything else built so far: every prior lowering (7a's whole selector, and this slice's hint/lea work) operates strictly within `select_inst`'s per-`Value` dispatch; diamond fusion instead needs to recognize a **multi-block CFG shape** *before* the main per-block walk and skip/merge three blocks' worth of normal lowering into one instruction — a real architectural addition, not an incremental match-arm. Bundling it into this slice would risk destabilizing 7a's clean, already-tested per-instruction model for a feature with no way to even be benchmarked yet (there's no working end-to-end pipeline until Phase 8 exists). Given a concrete name and slot (**Phase 7f**, tracked alongside 7c/7d/7e) rather than an open-ended "future slice," to be built once Phase 8 exists and diamond-fusion's actual performance value can be measured, not just assumed.

## Architecture

Both pieces live in `crates/forge-x64/src/machine_inst.rs`, extending `Selector`/`SelectedFunction` from 7a — no new files, no new crate dependencies.

**Coalescing hints** are a new `SelectedFunction::coalescing_hints: HashMap<Value, Value>` field (`dst -> preferred-same-location-as`), populated by a new pass `compute_coalescing_hints(insts: &[MachineInst]) -> HashMap<Value, Value>` that runs once, after `select_inst`/`select_term` have produced the full `Vec<MachineInst>`. For every 2-address-destructive `MachineInst` variant (binary: `IntAdd`/`IntSub`/`IntMul`/`And`/`Or`/`Xor`/`Shl`/`Shr`/`Sar`/`FloatAdd`/`FloatSub`/`FloatMul`/`FloatDiv`/`FloatMin`/`FloatMax`, where the real x86 instruction computes `dst = dst OP rhs` and so wants `dst`'s register to already hold `lhs`'s value; unary: `IntNeg`/`Not`/`FloatNeg`/`FloatAbs`, where `dst` wants to already hold `src`'s value), record `dst -> lhs` (or `dst -> src`). `IntDiv`/`IntRem` are excluded — their real hardware constraint is fixed `RAX`/`RDX` placement, not "same register as an operand," a different kind of hint Phase 8 will need to handle as a *fixed-register* constraint (already anticipated by CHECKLIST's Phase 8 `Interval.fixed` field), not a coalescing one. This is purely a lookup table — it doesn't change `insts` at all, matching 7a's principle that `MachineInst` stays 3-address SSA form throughout Phase 7.

**Scope note on CHECKLIST's literal `a + b*k + c` wording**: `forge_ir::Inst::Add` is strictly binary, so a genuine three-additive-term chain (`Add(Add(a, Mul(b,k)), c)` or similar, spanning two real `Add` instructions) is NOT recognized by this design — only the two-term shape `Add(Mul(b,k), c)` is. This is a reasonable, explicit scope reduction (the two-term case is both the common one and the one CHECKLIST's own bullet title emphasizes — "`lea` synthesis for `a + b*k + c`" names the general x86 addressing-mode shape `lea` supports, not a specific requirement that every use of it be recognized), not an oversight.

**`lea` synthesis** extends `select_inst`'s existing `Inst::Add` arm (7a's `Ty`-dispatching match). Before falling through to the existing `IntAdd`/`FloatAdd` dispatch, the `I64` case is checked against two tree shapes: `Add(Mul(b, ConstI64(k)), c)` and `Add(c, Mul(b, ConstI64(k)))`, for `k ∈ {2, 4, 8}` (`lea`'s only legal SIB scale values — `k=1` is deliberately excluded, since scale=1 buys nothing over a plain `IntAdd` and isn't worth the special-casing). Recognizing the shape means looking at an *operand's defining instruction* (`func.insts[operand.0 as usize]`, via a shared free function — see below) for the first time in this selector — 7a's arms only ever looked at an operand's *type* (`ty_of`), never its *defining instruction*. When the shape matches, `select_inst` emits `MachineInst::Lea { dst, base: c, index: b, scale: k, disp: 0 }` instead of `IntAdd`. That fusion, on its own, is only half the story — see the next section for why the fused `Mul`'s own computation must also be suppressed.

## Lea synthesis must suppress the fused `Mul`'s own computation — this is genuinely required, not an optional refinement

An earlier draft of this design assumed it was acceptable to leave the fused `Mul`'s own `IntMul` in place (a "redundant but harmless" simplification). That assumption doesn't hold, for two independent reasons surfaced during design review:

1. **It's not "sometimes redundant," it's "always dead."** `select()` unconditionally visits every real IR `Value` in RPO order and calls `select_inst` on it — nothing skips a `Value`'s own selection based on how a later instruction chooses to consume it. Since SSA def-before-use guarantees a `Mul`'s defining position is visited before any `Add` that might fuse it, the `Mul` **always** gets its own standalone `IntMul` pushed, whether or not it also gets fused into a `Lea`. Once fused, nothing downstream references that `IntMul`'s `dst` — it's unconditionally dead on every single successful fusion, not just when the `Mul`'s result happens to be shared. Leaving this unaddressed would mean `lea` synthesis produces `IntMul` (dead) *plus* `Lea` — never "one instruction instead of two," contradicting the entire point of this bullet.
2. **The "shared Mul" case is the common case, not a rare edge case.** `crates/forge-opt/src/gvn.rs` performs dominator-scoped CSE that canonicalizes commutative ops (including `Mul`, sorting operands by `Value` index) and merges syntactically-identical instructions within a dominating scope — confirmed by its own test (`repeated_subexpression_cses_to_one_add`). So two occurrences of the same `b*4`-shaped subexpression in a dominating scope are *already merged into one shared `Value`* by the time this selector runs. Un-suppressed, that shared `Mul` becomes a strict regression when both its uses fuse: the same instruction count as before, but one of them (the standalone `IntMul`) is now pure waste with no compensating benefit (each `Lea` still redoes its own scale-multiply independently — `lea`'s address computation doesn't consume a materialized `Mul` result at all, it recomputes from raw registers).

**The fix**: a whole-function analysis pass, run once before the main RPO walk, determines which `Mul` values are *fully* subsumed by fusion (every one of their uses is a fusable `Add` pattern — none "escape" to some other consumer) and are therefore safe to suppress entirely, the same way `Phi` is suppressed. This is, not incidentally, a more literal reading of CHECKLIST's own wording for this bullet-group ("maximal munch over the IR **DAG**," not "over the IR tree") — recognizing when a DAG node is fully consumed by a pattern match, not just tree-shaped consumption, is exactly what "over the DAG" implies.

The analysis:
1. Compute **total use count** for every real IR `Value` — walking every real instruction's operands (`forge_ir::uses_of`, which covers `Inst` but NOT `Terminator`) *plus* every block's terminator operand (`Terminator::Return(v)`'s `v`, `Terminator::Branch{cond,..}`'s `cond` — easy to miss, since `uses_of` doesn't cover these, and a `Value` that's directly `return`ed or branched-on must never be silently suppressed).
2. Compute **fusable use count** for every real IR `Value` by walking every real `Inst::Add(a, b)` where the type is `I64` and applying the *exact same* shape-matching logic `select_inst` will use (a single shared helper function, not two independent implementations that could drift out of sync) to determine which ONE of `a`/`b` (if either) this `Add` would fuse — incrementing that one `Value`'s fusable-use count. (When *both* operands are individually Mul-by-pow2-shaped, e.g. `Add(Mul(x,4), Mul(y,8))`, only one becomes the fused `index` operand per the `a`-then-`b` preference order below; the other remains an ordinary `Value` reference used as the `Lea`'s `base` operand and must NOT be counted as fusable — it still needs its own independent computation.)
3. A `Value` is safe to suppress exactly when `fusable_uses[v] == total_uses[v]` (every use without exception was absorbed by a fusion) — collected into `Selector::fully_fusable_muls: HashSet<Value>`, computed once and stored on `Selector` alongside `func`/`next_value`.
4. The **existing** `Inst::Mul` dispatch arm (shared with `Add`/`Sub`/`Div`/`Rem` per Task 2's `Ty`-dispatch pattern from 7a) gains one new check: for the `I64` case, if `self.fully_fusable_muls.contains(&dst)` (`dst` being *this* `Mul` instruction's own defining `Value`), emit nothing — exactly like `Phi`'s no-op arm — instead of `IntMul`.

This makes `lea` synthesis genuinely redundancy-free for every case the analysis can prove safe, while remaining conservative (never suppresses a `Mul` with ANY non-fusable use) and correct by construction (the suppression decision and the fusion decision are driven by the same shared shape-matcher, so they can never disagree about which operand is "the fused one").

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

`select(func)` (7a's entry point) calls this once at the end, populating `SelectedFunction::coalescing_hints`. `MachineInst::Lea` deliberately does NOT appear in `compute_coalescing_hints`'s match (it falls into the `_ => {}` arm) — this is intentional, not an omission: real x86 `lea` is a genuinely non-destructive 3-operand instruction (`dst` doesn't need to start out holding any operand's value), so it has no two-address constraint to hint around at all.

### `MachineInst::Lea`, the shared shape-matcher, the suppression pre-pass, and `select_inst`

```rust
// New MachineInst variant, added near the integer-arithmetic group
Lea { dst: Value, base: Value, index: Value, scale: u8, disp: i32 },
```

```rust
/// Free function (not a Selector method) so it has a single call site
/// usable both by the whole-function suppression pre-pass (which runs
/// BEFORE any Selector exists) and by Selector::try_synthesize_lea during
/// the main walk -- the suppression decision and the fusion decision MUST
/// agree about which operand (if any) is "the fused one," so there is
/// exactly one implementation of this shape check, not two that could
/// drift out of sync.
///
/// Checks whether `mul_candidate` is a real IR value (an index into
/// func.insts -- synthetic values are never Mul-defined and always
/// return None here) defined by `Mul(index, ConstI64(k))` or
/// `Mul(ConstI64(k), index)` for k in {2,4,8}; if so, returns
/// (base, index, k) with `base` set to the OTHER argument passed in.
fn match_mul_by_pow2(func: &Function, mul_candidate: Value, other: Value) -> Option<(Value, Value, u8)> {
    if (mul_candidate.0 as usize) >= func.insts.len() {
        return None;
    }
    let (m_a, m_b) = match &func.insts[mul_candidate.0 as usize] {
        Inst::Mul(x, y) => (*x, *y),
        _ => return None,
    };
    let scale_from = |v: Value| -> Option<u8> {
        if (v.0 as usize) >= func.insts.len() {
            return None;
        }
        match &func.insts[v.0 as usize] {
            Inst::ConstI64(k) if matches!(k, 2 | 4 | 8) => Some(*k as u8),
            _ => None,
        }
    };
    if let Some(k) = scale_from(m_b) {
        return Some((other, m_a, k));
    }
    if let Some(k) = scale_from(m_a) {
        return Some((other, m_b, k));
    }
    None
}

/// Tries both operand orderings of an Add(a, b) for the Mul-by-pow2-scale
/// shape, preferring `a` as the fused Mul if both individually qualify
/// (Add(Mul(x,4), Mul(y,8)) picks x/4 as index, leaving y's Mul as an
/// ordinary Value feeding the Lea's `base` -- NOT itself fused/suppressed).
fn find_fusable_add(func: &Function, a: Value, b: Value) -> Option<(Value, Value, u8)> {
    match_mul_by_pow2(func, a, b).or_else(|| match_mul_by_pow2(func, b, a))
}

/// Run once, before the main RPO walk, over the WHOLE function. Determines
/// which real IR Values, if any, are Mul results fully subsumed by lea
/// fusion (every use is a fusable Add pattern, none escape to any other
/// consumer) and therefore safe to suppress the same way Phi is.
fn find_fully_fusable_muls(func: &Function) -> std::collections::HashSet<Value> {
    use std::collections::HashMap;
    let mut total_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        for used in forge_ir::uses_of(inst) {
            *total_uses.entry(used).or_insert(0) += 1;
        }
    }
    // uses_of only covers Inst, never Terminator -- a directly-returned or
    // branched-on Value must still count as used, or it would be wrongly
    // suppressed even though the terminator needs its real computed value.
    for block in &func.blocks {
        match &block.term {
            Some(Terminator::Return(v)) => *total_uses.entry(*v).or_insert(0) += 1,
            Some(Terminator::Branch { cond, .. }) => *total_uses.entry(*cond).or_insert(0) += 1,
            _ => {}
        }
    }

    // NOTE: this must key on the OUTER Add's own operand (`a` or `b` --
    // one of which IS the Mul's defining Value) that matched, NOT on
    // find_fusable_add's returned `index` (which is the raw scaled
    // register INSIDE the matched Mul, e.g. `b` in `Mul(b, 4)` -- a
    // completely different Value that happens to share a variable name
    // with this comment's own `b` in `Add(a, b)`). Keying on the wrong
    // Value here silently defeats suppression entirely -- verified by
    // hand-tracing during design review that the naive
    // `find_fusable_add(...).map(|(_, index, _)| index)` version fails
    // to suppress ANY case, including the trivial single-consumer one.
    let mut fusable_uses: HashMap<Value, u32> = HashMap::new();
    for inst in &func.insts {
        if let Inst::Add(a, b) = inst {
            if match_mul_by_pow2(func, *a, *b).is_some() {
                *fusable_uses.entry(*a).or_insert(0) += 1;
            } else if match_mul_by_pow2(func, *b, *a).is_some() {
                *fusable_uses.entry(*b).or_insert(0) += 1;
            }
        }
    }

    total_uses
        .into_iter()
        .filter(|(v, total)| fusable_uses.get(v).copied().unwrap_or(0) == *total)
        .map(|(v, _)| v)
        .collect()
}
```

**A pathological but SSA-legal shape worth noting explicitly**: `Add(Mul_v, Mul_v)` (the same `Value` as both operands of one `Add`). `uses_of` counts this as 2 total uses of `Mul_v`; the fusable-use loop above only ever increments `Mul_v`'s count by at most 1 per `Add` instruction (it matches `a` XOR `b`, via the `if`/`else if`, never both). So `fusable_uses[Mul_v] < total_uses[Mul_v]` here, and `Mul_v` is correctly never suppressed — which is exactly right, since the synthesized `Lea`'s `base` field is set to `other = Mul_v` itself in this shape, so `Mul_v`'s real computed register genuinely still needs to exist at runtime. Not a case requiring special-case code; called out here because it's easy to worry about and the algorithm already handles it correctly by construction.

```rust
// Selector gains one new field:
struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
    fully_fusable_muls: std::collections::HashSet<Value>, // NEW
}
```

```rust
// select()'s setup, extended -- find_fully_fusable_muls(func) MUST run
// BEFORE the Selector is constructed (it needs a value at construction
// time, and takes &Function directly, not &Selector -- there is no
// Selector yet at this point). select()'s final return is also extended
// to populate coalescing_hints, computed from the now-complete insts list.
pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_muls = find_fully_fusable_muls(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_muls,
    };
    for block in forge_ir::dominance::reverse_postorder(func) {
        for &v in &func.blocks[block.0 as usize].insts {
            let inst = &func.insts[v.0 as usize];
            sel.select_inst(v, inst);
        }
        if let Some(term) = &func.blocks[block.0 as usize].term {
            sel.select_term(term);
        }
    }
    let coalescing_hints = compute_coalescing_hints(&sel.insts);
    SelectedFunction {
        insts: sel.insts,
        synthetic_types: sel.synthetic_types,
        coalescing_hints,
    }
}
```

```rust
// select_inst's Inst::Add arm, extended -- the F64/Bool cases are
// unchanged from 7a; only the I64 case gains a shape check first.
Inst::Add(a, b) => match self.ty_of(*a) {
    Ty::F64 => self.insts.push(MachineInst::FloatAdd { dst, lhs: *a, rhs: *b }),
    Ty::I64 => match find_fusable_add(self.func, *a, *b) {
        Some((base, index, scale)) => {
            self.insts.push(MachineInst::Lea { dst, base, index, scale, disp: 0 })
        }
        None => self.insts.push(MachineInst::IntAdd { dst, lhs: *a, rhs: *b }),
    },
    Ty::Bool => unreachable!("Add never applies to Bool"),
},
```

```rust
// select_inst's EXISTING Inst::Mul arm (from 7a Task 2) gains one new
// check in its I64 branch -- suppress if this Mul's own Value was fully
// subsumed by fusion, exactly like Phi's no-op arm.
Inst::Mul(a, b) => match self.ty_of(*a) {
    Ty::F64 => self.insts.push(MachineInst::FloatMul { dst, lhs: *a, rhs: *b }),
    Ty::I64 => {
        if !self.fully_fusable_muls.contains(&dst) {
            self.insts.push(MachineInst::IntMul { dst, lhs: *a, rhs: *b });
        }
        // else: fully subsumed by lea fusion, nothing to emit -- same
        // suppression discipline as Inst::Phi.
    }
    Ty::Bool => unreachable!("Mul never applies to Bool"),
},
```

## Testing

Golden `Vec<MachineInst>` tests, same style as 7a:
- `compute_coalescing_hints`: one test covering a representative binary op (proves `dst -> lhs`, not `dst -> rhs`), one for a unary op, one confirming `IntDiv`/`IntRem` produce NO hint entry, one confirming a `MachineInst` with no natural hint (e.g. `Param`, `Jump`) is correctly absent from the map.
- `lea` synthesis, single-consumer case: `Add(Mul(b,4), c)` and `Add(c, Mul(b,4))` (operand-order symmetry) each assert the FULL selected sequence has NO `IntMul` for the `Mul`'s `Value` at all — only the `Lea` — proving suppression actually happens, not just that fusion happens.
- `lea` synthesis, negative cases (no `Lea`, ordinary `IntMul`+`IntAdd`, no suppression): multiplier isn't a power-of-2-in-{2,4,8} constant (e.g. `Mul(b, 3)`); the "constant" operand is itself non-constant (e.g. `Mul(b, c)`, both real values).
- `lea` synthesis, shared-`Mul` case (the GVN-realistic scenario): one `Mul(b,4)` `Value` consumed by TWO different `Add`s (`Add(Mul_v, c1)` and `Add(Mul_v, c2)`) — asserts BOTH `Add`s become `Lea`s and the `Mul`'s own `IntMul` is still fully suppressed (both uses were fusable, so `fusable_uses == total_uses`), proving the suppression pass correctly handles the multi-consumer case, not just the single-consumer one.
- `lea` synthesis, escaping-use case (suppression must NOT happen): a `Mul(b,4)` `Value` used by one fusable `Add` AND also directly `Return`ed (or used by a second, non-`Add` consumer) — asserts the `Lea` is still emitted for the `Add`, but the `Mul`'s own `IntMul` is ALSO still present (since not every use was fusable) — this is the test that would have caught the original design flaw, and specifically exercises the terminator-use-counting fix (a `Value` used only by `Return` must never be wrongly suppressed).
- `lea` synthesis, both-operands-fusable case: `Add(Mul(x,4), Mul(y,8))` — asserts exactly one of the two (per the `a`-then-`b` preference order) becomes the `Lea`'s `index`, the other remains an ordinary `Value` reference as `base` and gets its own independent, non-suppressed `IntMul`.

## Exit criteria

1. `SelectedFunction::coalescing_hints` exists, populated by `compute_coalescing_hints`, covering every 2-address-destructive `MachineInst` variant, correctly excluding `IntDiv`/`IntRem`.
2. `MachineInst::Lea` exists; `select_inst`'s `Inst::Add`/`I64` case recognizes `Add(Mul(b,k),c)`/`Add(c,Mul(b,k))` for `k ∈ {2,4,8}` and emits it via the shared `find_fusable_add` helper; falls back to plain `IntAdd` for every other shape.
3. `find_fully_fusable_muls` correctly suppresses a fused `Mul`'s standalone `IntMul` exactly when every one of its uses (across both `Inst` operands AND block terminators) was absorbed by fusion, and never suppresses it otherwise.
4. Tests cover both operand orderings, both negative cases, the shared-consumer suppression case, the escaping-use non-suppression case, and the both-operands-fusable preference-order case.
5. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
6. No regressions in any Phase 6 `forge-x64` test, Phase 7a's `machine_inst` tests, or any other crate's tests.
7. CHECKLIST.md's "Two-address fixup," "Addressing-mode folding," and "`lea` synthesis" bullets are annotated to reflect what was actually built (or explicitly why not, for addressing-mode folding), and the "`Select`→`cmov`" bullet is annotated as deferred to the newly-named Phase 7f.
