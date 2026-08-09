# forge Phase 7c Constant Pool & Sign-Mask Constants Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deduplicating `ConstantPool` to `forge-x64`'s instruction selector, and change `MachineInst::LoadImmF64`/`FloatAbs`/`FloatNeg` to reference pool entries instead of embedding raw bits / minting a synthetic materialize-via-GPR sequence.

**Architecture:** `ConstantPool`/`PoolIndex` are new types in `crates/forge-x64/src/machine_inst/mod.rs`. `Selector` gains a `pool: ConstantPool` field; `SelectedFunction` gains `pub pool: ConstantPool`. Three existing `select_inst` arms change (`Inst::ConstF64`, `Inst::Abs`, `Inst::Neg`'s F64 branch) to intern into the pool instead of embedding bits or minting a synthetic `Value`. `Fma`'s unrelated `mul_tmp` mechanism is untouched.

**Tech Stack:** Rust. No new dependencies.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-7c-constant-pool-design.md` — read this first, especially the section on why `intern` deliberately dedupes across "different kinds" of constant (the `-0.0`/`Neg`-mask collision) — this is a real, worked-through invariant, not a hypothetical.

**A note on running test counts:** this plan modifies 3 existing tests in `crates/forge-x64/src/machine_inst/tests.rs` (enumerated exactly in Task 1/2 below, confirmed to be the complete list via a design-review grep of the whole file) in addition to adding new ones. Trust `cargo test -p forge-x64 --lib`'s actual output over any running-count arithmetic in this plan.

---

## Task 1: `ConstantPool`/`PoolIndex`, `LoadImmF64` change, deduplication tests

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-x64/src/machine_inst/tests.rs`
- Modify: `crates/forge-x64/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst/tests.rs — REPLACE the existing
// select_lowers_an_f64_constant test (it currently asserts against the
// old `bits: u64` field, which no longer exists after this task) with:

#[test]
fn select_lowers_an_f64_constant() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let bits = 3.5f64.to_bits();
    let c = b.emit(entry, Inst::ConstF64(bits), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(c));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::LoadImmF64 {
                dst: c,
                pool_index: PoolIndex(0)
            },
            MachineInst::Return { value: c },
        ]
    );
    assert_eq!(selected.pool.entries(), &[bits]);
}

#[test]
fn select_dedups_identical_f64_literals_into_one_pool_entry() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let bits = 0.5f64.to_bits();
    let c1 = b.emit(entry, Inst::ConstF64(bits), Ty::F64, dummy_span());
    let c2 = b.emit(entry, Inst::ConstF64(bits), Ty::F64, dummy_span());
    let sum = b.emit(entry, Inst::Add(c1, c2), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[0],
        MachineInst::LoadImmF64 {
            dst: c1,
            pool_index: PoolIndex(0)
        }
    );
    assert_eq!(
        selected.insts[1],
        MachineInst::LoadImmF64 {
            dst: c2,
            pool_index: PoolIndex(0)
        }
    );
    assert_eq!(selected.pool.entries().len(), 1);
}

#[test]
fn select_gives_different_f64_literals_different_pool_entries() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let c1 = b.emit(entry, Inst::ConstF64(1.0f64.to_bits()), Ty::F64, dummy_span());
    let c2 = b.emit(entry, Inst::ConstF64(2.0f64.to_bits()), Ty::F64, dummy_span());
    let sum = b.emit(entry, Inst::Add(c1, c2), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    let pool_index_of = |v: Value| match selected.insts.iter().find(|i| {
        matches!(i, MachineInst::LoadImmF64 { dst, .. } if *dst == v)
    }) {
        Some(MachineInst::LoadImmF64 { pool_index, .. }) => *pool_index,
        _ => panic!("expected a LoadImmF64 for {:?}", v),
    };
    assert_ne!(pool_index_of(c1), pool_index_of(c2));
    assert_eq!(selected.pool.entries().len(), 2);
}
```

**IMPORTANT**: the first test replaces an existing one — don't leave the old `bits`-based version alongside it; the old field no longer exists after Step 3, so the old test won't compile.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — `PoolIndex`/`ConstantPool` don't exist yet, `LoadImmF64` still has `bits` not `pool_index` (compile errors).

- [ ] **Step 3: Add `ConstantPool`/`PoolIndex`, change `LoadImmF64`, wire into `Selector`/`select()`**

```rust
// crates/forge-x64/src/machine_inst/mod.rs — add near the top, after the
// existing use statements, before MachineInst

/// An index into a ConstantPool's entries. Opaque outside this module,
/// same "not usable across a different pool" caveat as Label is for a
/// different Assembler (Phase 6a's precedent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PoolIndex(usize);

/// Deduplicating storage for the 8-byte constants (f64 literals, sign
/// masks) instruction selection needs a real memory location for.
/// Actual byte layout / RIP-relative addressing happens later, once a
/// real Assembler and PhysReg assignments exist (post-Phase-8) -- this
/// is purely the "what constants exist, and which ones are the same
/// value" bookkeeping.
///
/// `intern` dedupes on the raw u64 bit pattern alone, deliberately not
/// distinguishing "this came from an f64 literal" from "this is an
/// integer bitmask" -- e.g. i64::MIN's bits equal -0.0f64's bits, and a
/// function using both will correctly share one pool entry between them.
/// This is safe: the pool is a passive, read-only byte store, and the
/// LOAD STRATEGY is determined entirely by which MachineInst variant
/// references a PoolIndex (LoadImmF64 always means "load as f64 value";
/// FloatAbs/FloatNeg's mask_pool always means "load as bitmask"), never
/// by inspecting the pool entry itself. Keying on raw bits rather than
/// f64 equality is also what correctly keeps +0.0 (0x0) and -0.0
/// (0x8000...0) in separate entries despite comparing equal under `==`.
#[derive(Default)]
pub struct ConstantPool {
    entries: Vec<u64>,
    index_of: HashMap<u64, PoolIndex>,
}

impl ConstantPool {
    /// Returns the existing PoolIndex if `bits` was already interned,
    /// otherwise appends a new entry and returns its index. This is the
    /// ONLY way constants enter the pool -- callers never construct a
    /// PoolIndex directly (outside this module/its test submodule).
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

```rust
// crates/forge-x64/src/machine_inst/mod.rs — change the LoadImmF64
// variant's field (was `bits: u64`)

    LoadImmF64 { dst: Value, pool_index: PoolIndex },
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — SelectedFunction gains a
// pool field

pub struct SelectedFunction {
    pub insts: Vec<MachineInst>,
    pub synthetic_types: HashMap<Value, Ty>,
    pub coalescing_hints: HashMap<Value, Value>,
    pub pool: ConstantPool,
}
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — Selector gains a pool field

struct Selector<'a> {
    func: &'a Function,
    insts: Vec<MachineInst>,
    synthetic_types: HashMap<Value, Ty>,
    next_value: u32,
    fully_fusable_scaled_indices: std::collections::HashSet<Value>,
    pool: ConstantPool,
}
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — select_inst's Inst::ConstF64 arm

            Inst::ConstF64(bits) => {
                let pool_index = self.pool.intern(*bits);
                self.insts.push(MachineInst::LoadImmF64 { dst, pool_index });
            }
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — select()'s setup and return,
// extended for `pool` (Selector's default-constructed via ConstantPool's
// Default impl; select()'s final return threads sel.pool through)

pub fn select(func: &Function) -> SelectedFunction {
    let fully_fusable_scaled_indices = find_fully_fusable_scaled_indices(func);
    let mut sel = Selector {
        func,
        insts: Vec::new(),
        synthetic_types: HashMap::new(),
        next_value: func.insts.len() as u32,
        fully_fusable_scaled_indices,
        pool: ConstantPool::default(),
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
        pool: sel.pool,
    }
}
```

- [ ] **Step 4: Update `lib.rs` exports**

```rust
// crates/forge-x64/src/lib.rs — full file contents

mod assembler;
mod machine_inst;
mod reg;

pub use assembler::{AluOp, Assembler, ConditionCode, Label, RoundMode, ShiftOp, SseOp};
pub use machine_inst::{select, ConstantPool, MachineInst, PoolIndex, SelectedFunction};
pub use reg::PhysReg;
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -40`
Expected: all 3 tests from Step 1 pass. This will also surface compile errors in `select_lowers_float_neg_via_a_synthetic_mask_temp` and `select_lowers_abs_via_a_synthetic_mask_temp` (they don't reference `LoadImmF64`, but `FloatAbs`/`FloatNeg` still have their OLD `mask_tmp: Value` shape at this point in the plan, so those two tests should still compile and pass unchanged — Task 2 changes them). If those two tests fail to compile, STOP and report — that would mean this task accidentally touched something Task 2 owns.

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs crates/forge-x64/src/machine_inst/tests.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): ConstantPool, PoolIndex, LoadImmF64 references pool entries"
```

## Context for this task

This task establishes `ConstantPool`/`PoolIndex` and converts `LoadImmF64` — the simpler of the two conversions this plan makes (no synthetic-`Value`-minting machinery to remove, unlike `FloatAbs`/`FloatNeg` in Task 2). `select_lowers_an_f64_constant`, `select_lowers_float_neg_via_a_synthetic_mask_temp`, and `select_lowers_abs_via_a_synthetic_mask_temp` are the ONLY three existing tests in the whole file referencing the shapes this plan changes (confirmed exhaustively via `grep` during design review) — this task touches the first, Task 2 touches the other two.

`PoolIndex(0)` construction works directly in test code because `tests.rs` is a child module of `machine_inst` (`mod tests;` inside `mod.rs`), and Rust privacy allows descendant modules to see private fields of types defined in an ancestor module — no `pub` needed on `PoolIndex`'s inner field for this to compile.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `FloatAbs`/`FloatNeg` reference pool entries, sharing/collision tests, `Fma`-unchanged confirmation

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-x64/src/machine_inst/tests.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/forge-x64/src/machine_inst/tests.rs — REPLACE
// select_lowers_float_neg_via_a_synthetic_mask_temp with:

#[test]
fn select_lowers_float_neg_via_a_pooled_mask() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Neg(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[1],
        MachineInst::FloatNeg {
            dst: r,
            src: x,
            mask_pool: PoolIndex(0)
        }
    );
    assert_eq!(selected.pool.entries(), &[i64::MIN as u64]);
    // No synthetic Value minted anymore -- the old mask-temp mechanism
    // is gone for this case (Fma's mul_tmp mechanism, tested separately
    // below, is unrelated and still mints synthetic values).
    assert!(selected.synthetic_types.is_empty());
}

// crates/forge-x64/src/machine_inst/tests.rs — REPLACE
// select_lowers_abs_via_a_synthetic_mask_temp with:

#[test]
fn select_lowers_abs_via_a_pooled_mask() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Abs(x), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[1],
        MachineInst::FloatAbs {
            dst: r,
            src: x,
            mask_pool: PoolIndex(0)
        }
    );
    assert_eq!(selected.pool.entries(), &[0x7FFF_FFFF_FFFF_FFFFu64]);
    assert!(selected.synthetic_types.is_empty());
}

#[test]
fn select_shares_one_pool_entry_across_multiple_abs_call_sites() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let abs_x = b.emit(entry, Inst::Abs(x), Ty::F64, dummy_span());
    let abs_y = b.emit(entry, Inst::Abs(y), Ty::F64, dummy_span());
    let sum = b.emit(entry, Inst::Add(abs_x, abs_y), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts[2],
        MachineInst::FloatAbs {
            dst: abs_x,
            src: x,
            mask_pool: PoolIndex(0)
        }
    );
    assert_eq!(
        selected.insts[3],
        MachineInst::FloatAbs {
            dst: abs_y,
            src: y,
            mask_pool: PoolIndex(0)
        }
    );
    assert_eq!(selected.pool.entries().len(), 1);
}

#[test]
fn select_abs_and_neg_masks_do_not_share_a_pool_entry() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let abs_x = b.emit(entry, Inst::Abs(x), Ty::F64, dummy_span());
    let neg_y = b.emit(entry, Inst::Neg(y), Ty::F64, dummy_span());
    let sum = b.emit(entry, Inst::Add(abs_x, neg_y), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    assert_eq!(selected.pool.entries().len(), 2);
    assert!(selected.pool.entries().contains(&0x7FFF_FFFF_FFFF_FFFFu64));
    assert!(selected.pool.entries().contains(&(i64::MIN as u64)));
}

/// The case worked through explicitly in the design doc: i64::MIN's bits
/// equal -0.0f64's bits, so a function with BOTH a literal -0.0 and a
/// float Neg deliberately shares one pool entry between them. This is
/// intended, correct behavior -- not a bug to fix if you're reading this
/// after a "why does this collide" investigation.
#[test]
fn select_negative_zero_literal_and_neg_mask_deliberately_collide() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let neg_zero = b.emit(entry, Inst::ConstF64((-0.0f64).to_bits()), Ty::F64, dummy_span());
    let y = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let neg_y = b.emit(entry, Inst::Neg(y), Ty::F64, dummy_span());
    let sum = b.emit(entry, Inst::Add(neg_zero, neg_y), Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(sum));

    let selected = select(&b.f);

    assert_eq!(selected.pool.entries().len(), 1);
    assert_eq!(selected.pool.entries()[0], i64::MIN as u64);
}

#[test]
fn select_fma_temp_mechanism_is_unaffected_by_the_constant_pool() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let y = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let z = b.emit(
        entry,
        Inst::Param {
            index: 2,
            ty: Ty::F64,
        },
        Ty::F64,
        dummy_span(),
    );
    let r = b.emit(entry, Inst::Fma { a: x, b: y, c: z }, Ty::F64, dummy_span());
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    let mul_tmp = match &selected.insts[3] {
        MachineInst::FloatMul { dst, lhs, rhs } => {
            assert_eq!(*lhs, x);
            assert_eq!(*rhs, y);
            *dst
        }
        other => panic!("expected FloatMul, got {:?}", other),
    };
    assert_eq!(
        selected.insts[4],
        MachineInst::FloatAdd {
            dst: r,
            lhs: mul_tmp,
            rhs: z
        }
    );
    assert_eq!(selected.synthetic_types.get(&mul_tmp), Some(&Ty::F64));
    assert!(selected.pool.entries().is_empty());
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — `FloatAbs`/`FloatNeg` still have `mask_tmp: Value`, not `mask_pool: PoolIndex` (compile errors on the new/replaced tests).

- [ ] **Step 3: Change `FloatAbs`/`FloatNeg` and their `select_inst` arms**

```rust
// crates/forge-x64/src/machine_inst/mod.rs — change both variant field lists

    FloatAbs { dst: Value, src: Value, mask_pool: PoolIndex },
    FloatNeg { dst: Value, src: Value, mask_pool: PoolIndex },
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — Inst::Abs's arm, replacing
// the fresh()/LoadImmI64 sequence

            Inst::Abs(a) => {
                // 0x7FFF_FFFF_FFFF_FFFF: every bit set EXCEPT the sign bit.
                // AND-ing this into the value CLEARS the sign bit
                // (absolute value) -- contrast Neg's mask below, which
                // FLIPS it via XOR instead.
                let mask_pool = self.pool.intern(0x7FFF_FFFF_FFFF_FFFFu64);
                self.insts.push(MachineInst::FloatAbs { dst, src: *a, mask_pool });
            }
```

```rust
// crates/forge-x64/src/machine_inst/mod.rs — Inst::Neg's F64 branch,
// replacing the fresh()/LoadImmI64 sequence

                Ty::F64 => {
                    // i64::MIN == 0x8000_0000_0000_0000: only the sign bit
                    // set. XOR-ing this into the value FLIPS the sign bit
                    // (negation) -- contrast Abs's mask above, which CLEARS
                    // it via AND instead.
                    let mask_pool = self.pool.intern(i64::MIN as u64);
                    self.insts.push(MachineInst::FloatNeg { dst, src: *a, mask_pool });
                }
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -50`
Expected: all tests pass, including Task 1's tests and all of this task's new/replaced tests.

- [ ] **Step 5: Run the FULL workspace test suite to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 6: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs crates/forge-x64/src/machine_inst/tests.rs
git commit -m "feat(forge-x64): FloatAbs/FloatNeg reference shared pooled sign-mask constants"
```

## Context for this task

This is where the design's most interesting invariant gets exercised: `select_negative_zero_literal_and_neg_mask_deliberately_collide` is not a bug-guard, it's confirming INTENDED behavior — do not "fix" the implementation if this test's assertion (`pool.entries().len() == 1`) feels surprising; re-read the design doc's section on why cross-kind collision is safe before assuming something's wrong.

`select_fma_temp_mechanism_is_unaffected_by_the_constant_pool` confirms this task's changes are properly scoped — `Fma`'s `mul_tmp` still uses `Selector::fresh`/`synthetic_types` exactly as in Phase 7a, completely untouched by the pool.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -60`. Report exact final counts.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm all 9 exit criteria from the design doc are met:
1. `ConstantPool`/`PoolIndex` exist; `intern` deduplicates identical `u64` bit patterns and is the only way to obtain a `PoolIndex`.
2. `SelectedFunction::pool: ConstantPool` exists, populated from the `Selector`'s pool at the end of `select()`.
3. `MachineInst::LoadImmF64` carries `pool_index: PoolIndex` instead of `bits: u64`; `Inst::ConstF64`'s arm interns instead of embedding.
4. `MachineInst::FloatAbs`/`FloatNeg` carry `mask_pool: PoolIndex` instead of `mask_tmp: Value`; their arms intern the fixed mask constant instead of minting a synthetic `Value` + `LoadImmI64`.
5. All 3 existing tests referencing the old shapes are updated.
6. Tests cover single-constant interning, cross-call-site deduplication (f64 literals and shared masks), non-deduplication of different constants, the deliberate cross-kind collision, and confirm `Fma`'s unrelated `mul_tmp` mechanism is untouched.
7. `ConstantPool`/`PoolIndex` are re-exported from `lib.rs`.
8. `cargo test --workspace` green, clippy/fmt clean.
9. No regressions in any Phase 6 `forge-x64` test or any other crate's tests.
