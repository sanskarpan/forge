# forge Phase 7e libm Call Selection & Address Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `MachineInst::CallLibm`, replacing `select_inst`'s `Inst::Call => unimplemented!(...)` panic with a real (still virtual-register, SSA-form) selection arm, and add `libm_address(func: LibFunc) -> i64`, real FFI resolution of libm symbol addresses, in a new `crates/forge-x64/src/libm.rs`.

**Architecture:** Two independent pieces, split into two tasks. Task 1 (`libm.rs`) needs nothing from Task 2 and can be built/tested standalone. Task 2 (`MachineInst::CallLibm`) needs a `Cargo.toml` dependency change (`smallvec` promoted from dev- to real dependency) and touches `machine_inst/mod.rs` + `machine_inst/tests.rs`. The REAL byte-level call sequence (spilling, ABI arg marshalling, alignment, actual `call_reg` emission) is explicitly OUT of scope for both tasks — deferred to a separate, not-yet-started task that needs real Phase 8 register-allocation output.

**Tech Stack:** Rust, `extern "C"` FFI to the platform's libm, `smallvec`.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-7e-libm-call-design.md` — read this first. This design was reviewed with an execution-based pass (a scratch worktree with the exact proposed code applied, `cargo build`/`test`/`clippy`/`fmt` all run for real) — two real bugs were found and fixed in the design (a `clippy::fn_to_numeric_cast` violation, and an existing test that must be replaced) before this plan was written; trust the design's code as given below, it's already been executed successfully.

---

## Task 1: `libm_address` — real libm symbol resolution

**Files:**
- Create: `crates/forge-x64/src/libm.rs`
- Modify: `crates/forge-x64/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-x64/src/libm.rs` with ONLY this content for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Casts `addr` back through the appropriate function-pointer type and
    /// calls it -- this is what the future call-emission step effectively
    /// does at runtime via mov_reg_imm+call_reg, just done here in-process
    /// instead of through JIT-generated machine code.
    unsafe fn call_unary(addr: i64, x: f64) -> f64 {
        let f: unsafe extern "C" fn(f64) -> f64 = std::mem::transmute(addr as usize);
        f(x)
    }

    unsafe fn call_binary(addr: i64, x: f64, y: f64) -> f64 {
        let f: unsafe extern "C" fn(f64, f64) -> f64 = std::mem::transmute(addr as usize);
        f(x, y)
    }

    #[test]
    fn all_six_addresses_are_real_and_pairwise_distinct() {
        let addrs = [
            libm_address(LibFunc::Sin),
            libm_address(LibFunc::Cos),
            libm_address(LibFunc::Tan),
            libm_address(LibFunc::Exp),
            libm_address(LibFunc::Log),
            libm_address(LibFunc::Pow),
        ];
        for &a in &addrs {
            assert_ne!(a, 0, "resolved address must not be null");
        }
        for i in 0..addrs.len() {
            for j in (i + 1)..addrs.len() {
                assert_ne!(
                    addrs[i], addrs[j],
                    "addrs[{i}] and addrs[{j}] must be distinct libm symbols"
                );
            }
        }
    }

    /// Bit-exact, not approximate: this is the SAME underlying libm
    /// implementation Rust's own f64::sin/cos/tan/exp/ln/powf call into on
    /// every platform this project's CI targets, so exact equality is the
    /// correct, achievable bar (matching this project's FMA-vs-approximation
    /// precision discipline of never silently accepting "close enough" where
    /// exact is achievable).
    ///
    /// Test inputs are deliberately NOT special-cased exponents/identities
    /// (no 0.0, 1.0, 2.0, -1.0, or 0.5 exponents for Pow) -- see
    /// crates/forge-opt/src/strength.rs's own documented
    /// `LibCallSimplifier` hazard: LLVM recognizes calls literally named
    /// `pow`/`sin`/etc (even through a hand-written `extern "C"`
    /// declaration) and rewrites special-cased inputs to fmul/fdiv/sqrt at
    /// compile time, which would make comparing against `f64::powf` for
    /// exactly those inputs circular. `std::hint::black_box` on the f64::*
    /// oracle side is extra defense-in-depth against exactly that.
    #[test]
    fn resolved_addresses_are_bit_exact_against_rust_std_math() {
        use std::hint::black_box;

        for &x in &[0.5f64, 1.0, 2.0, -1.5] {
            unsafe {
                assert_eq!(
                    call_unary(libm_address(LibFunc::Sin), x),
                    black_box(x).sin()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Cos), x),
                    black_box(x).cos()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Tan), x),
                    black_box(x).tan()
                );
                assert_eq!(
                    call_unary(libm_address(LibFunc::Exp), x),
                    black_box(x).exp()
                );
            }
        }
        // Log: positive inputs only (ln of a negative number is NaN, not a
        // meaningful bit-exact comparison target).
        for &x in &[0.5f64, 1.0, 2.0] {
            unsafe {
                assert_eq!(
                    call_unary(libm_address(LibFunc::Log), x),
                    black_box(x).ln()
                );
            }
        }
        unsafe {
            assert_eq!(
                call_binary(libm_address(LibFunc::Pow), 2.0, 10.0),
                black_box(2.0f64).powf(black_box(10.0))
            );
        }
    }

    /// Confirms libc's `log` really is natural log (matching forge-ir's
    /// interpreter oracle, `LibFunc::Log => a.ln()` in interp.rs) and NOT
    /// base-10 log10 -- ln(e) ~= 1.0, log10(e) ~= 0.434.
    #[test]
    fn log_is_natural_log_not_log10() {
        unsafe {
            let result = call_unary(libm_address(LibFunc::Log), std::f64::consts::E);
            assert!(
                (result - 1.0).abs() < 1e-10,
                "expected ln(e) ~= 1.0, got {result}"
            );
        }
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Read the current `crates/forge-x64/src/lib.rs` first, then add exactly these two lines in the appropriate alphabetically-sorted spots (matching the existing style):
- `mod libm;` among the other `mod` declarations
- `pub use libm::libm_address;` among the other `pub use` lines

This must happen NOW, before Step 3's "confirm failure" — until `libm.rs` is referenced by a `mod` declaration, Rust never compiles it at all, so `cargo test` would silently see nothing rather than fail loudly.

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib 2>&1 | head -60`
Expected: FAIL — compile error (`libm_address`/`LibFunc` unresolved).

- [ ] **Step 4: Write the implementation**

Prepend this to the TOP of `crates/forge-x64/src/libm.rs`, above the `#[cfg(test)] mod tests` block from Step 1:

```rust
use forge_ir::LibFunc;

extern "C" {
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
    fn tan(x: f64) -> f64;
    fn exp(x: f64) -> f64;
    fn log(x: f64) -> f64;
    fn pow(x: f64, y: f64) -> f64;
}

/// Resolves `func`'s real, process-wide libm symbol to an absolute
/// address suitable for `Assembler::mov_reg_imm` (which already
/// auto-selects the 10-byte movabs form for any value that doesn't fit
/// i32 -- no new encoder support needed) followed by `call_reg` --
/// `call_reg`'s own doc comment in assembler.rs already documents why an
/// indirect call through a resolved absolute address is required here: a
/// direct rel32 call can't reliably reach libm from a JIT-allocated page,
/// whose distance from libc in the address space isn't bounded to
/// +/-2GiB.
///
/// C's `log` is natural log (matches forge-ir's interpreter oracle,
/// `LibFunc::Log => a.ln()` in interp.rs -- NOT base-10 log10).
pub fn libm_address(func: LibFunc) -> i64 {
    type Unary = unsafe extern "C" fn(f64) -> f64;
    type Binary = unsafe extern "C" fn(f64, f64) -> f64;
    // The extra `as usize` hop before `as i64` is required, not stylistic:
    // casting a function pointer directly to i64 trips clippy::fn_to_numeric_cast
    // (a default-warn lint under this project's -D warnings gate) -- casting
    // through usize first (the pointer-width unsigned integer type) is the
    // idiomatic way to convert a fn pointer to an integer without tripping it.
    match func {
        LibFunc::Sin => sin as Unary as usize as i64,
        LibFunc::Cos => cos as Unary as usize as i64,
        LibFunc::Tan => tan as Unary as usize as i64,
        LibFunc::Exp => exp as Unary as usize as i64,
        LibFunc::Log => log as Unary as usize as i64,
        LibFunc::Pow => pow as Binary as usize as i64,
    }
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -40`
Expected: all 4 new `libm::tests::*` tests pass (`all_six_addresses_are_real_and_pairwise_distinct`, `resolved_addresses_are_bit_exact_against_rust_std_math`, `log_is_natural_log_not_log10`), no regressions among the pre-existing tests.

- [ ] **Step 6: `cargo fmt` and `cargo clippy -p forge-x64 --all-targets -- -D warnings`, fix anything found**

The `as usize as i64` cast form in Step 4 is already clippy-clean per this plan's design review — if clippy still flags something, re-check the exact cast chain matches Step 4 verbatim before assuming a new issue.

- [ ] **Step 7: Commit**

```bash
git add crates/forge-x64/src/libm.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-x64): libm_address, real libm symbol resolution"
```

## Context for Task 1

`libm_address` doesn't depend on anything Task 2 adds — it's pure FFI address resolution, fully testable today (unlike the real call sequence, which needs Phase 8's register allocations and is out of scope for this whole plan). Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: `MachineInst::CallLibm`

**Files:**
- Modify: `crates/forge-x64/src/machine_inst/mod.rs`
- Modify: `crates/forge-x64/src/machine_inst/tests.rs`
- Modify: `crates/forge-x64/Cargo.toml`

- [ ] **Step 1: Promote `smallvec` from dev-dependency to a real dependency**

`crates/forge-x64/Cargo.toml` currently has:
```toml
[dependencies]
forge-ir = { path = "../forge-ir" }

[dev-dependencies]
iced-x86.workspace = true
forge-syntax = { path = "../forge-syntax" }
smallvec.workspace = true
```

`MachineInst::CallLibm`'s `args` field needs `smallvec::SmallVec` as a real (non-dev) type. Change to:
```toml
[dependencies]
forge-ir = { path = "../forge-ir" }
smallvec.workspace = true

[dev-dependencies]
iced-x86.workspace = true
forge-syntax = { path = "../forge-syntax" }
```

(The `smallvec.workspace = true` line moves out of `[dev-dependencies]` entirely — a regular dependency is automatically visible to the crate's own `#[cfg(test)]` code too, so leaving it in both places would be a redundant duplicate, not an addition.)

- [ ] **Step 2: Replace the obsolete panic test with a failing (not-yet-passing) real test**

In `crates/forge-x64/src/machine_inst/tests.rs`, find this existing test (it currently passes, asserting `Inst::Call` panics):

```rust
#[test]
#[should_panic(expected = "Phase 7e")]
fn select_panics_on_call_with_a_clear_deferral_message() {
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
    let r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![x],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    select(&b.f); // must panic
}
```

Delete it entirely and replace it with these three tests in its place:

```rust
#[test]
fn select_lowers_a_unary_libm_call() {
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
    let r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![x],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::Param { dst: x, index: 0 },
            MachineInst::CallLibm {
                dst: r,
                func: forge_ir::LibFunc::Sin,
                args: smallvec::smallvec![x],
            },
            MachineInst::Return { value: r },
        ]
    );
}

#[test]
fn select_lowers_a_binary_libm_call() {
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
    let r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Pow,
            args: smallvec::smallvec![x, y],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    assert_eq!(
        selected.insts,
        vec![
            MachineInst::Param { dst: x, index: 0 },
            MachineInst::Param { dst: y, index: 1 },
            MachineInst::CallLibm {
                dst: r,
                func: forge_ir::LibFunc::Pow,
                args: smallvec::smallvec![x, y],
            },
            MachineInst::Return { value: r },
        ]
    );
}

#[test]
fn coalescing_hints_no_entry_for_call_libm() {
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
    let r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![x],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(r));

    let selected = select(&b.f);

    // A call's dst isn't 2-address-destructive -- its real location is
    // wherever the ABI return convention places it (xmm0), unrelated to
    // any operand's register, so it must never get a coalescing hint.
    assert_eq!(selected.coalescing_hints.get(&r), None);
}
```

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test -p forge-x64 --lib -- select_lowers_a_unary_libm_call select_lowers_a_binary_libm_call coalescing_hints_no_entry_for_call_libm 2>&1 | tail -40`

(The `--` is required: cargo's own CLI accepts only one bare TESTNAME before it; everything after `--` is forwarded to the libtest binary, which does accept multiple filter substrings.)
Expected: FAIL — compile error (`MachineInst::CallLibm` doesn't exist yet).

- [ ] **Step 4: Add the `CallLibm` variant to `MachineInst`**

In `crates/forge-x64/src/machine_inst/mod.rs`, find the `MachineInst` enum's `FloatToInt` variant (the last entry of the "Conversions" section, immediately before the `// Control flow` section comment and `Jump` variant):

```rust
    FloatToInt {
        dst: Value,
        src: Value,
    }, // truncating (cvttsd2si)

    // Control flow
```

Insert a new section between them:

```rust
    FloatToInt {
        dst: Value,
        src: Value,
    }, // truncating (cvttsd2si)

    // libm calls -- see crates/forge-x64/src/libm.rs for address resolution.
    // Still fully virtual-register/SSA-form like every other MachineInst: the
    // real call SEQUENCE (spill live regs, marshal args into xmm0/xmm1, align
    // rsp, mov_reg_imm+call_reg, move the f64 result out of xmm0) is entirely
    // the future emission step's job, once Phase 8 assigns real registers --
    // this variant only records WHAT gets called, with WHICH SSA args, and
    // WHERE the result goes.
    CallLibm {
        dst: Value,
        func: forge_ir::LibFunc,
        args: smallvec::SmallVec<[Value; 2]>,
    },

    // Control flow
```

- [ ] **Step 5: Replace the `Inst::Call` panic arm**

In the same file, `select_inst`'s match currently has:

```rust
            Inst::Call { .. } => unimplemented!("libm call lowering ships in Phase 7e"),
```

Replace it with:

```rust
            Inst::Call { func, args } => {
                self.insts.push(MachineInst::CallLibm { dst, func: *func, args: args.clone() });
            }
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -60`
Expected: `select_lowers_a_unary_libm_call`, `select_lowers_a_binary_libm_call`, `coalescing_hints_no_entry_for_call_libm` all pass; the deleted `select_panics_on_call_with_a_clear_deferral_message` no longer appears anywhere; no regressions among the pre-existing tests.

- [ ] **Step 7: Add the fusable-scaled-index non-suppression regression test**

Append this test to `crates/forge-x64/src/machine_inst/tests.rs` (anywhere among the other `lea_synthesis_*` tests is a natural spot):

```rust
/// Mirrors lea_synthesis_escaping_use_prevents_suppression's structure,
/// but the SECOND (escaping) use is a libm call argument instead of a
/// direct Return -- this specifically exercises find_fully_fusable_
/// scaled_indices's reliance on forge_ir::uses_of's Inst::Call coverage.
/// Without that coverage, `mul`'s call-argument use wouldn't be counted
/// in total_uses, its only recorded use would be the fusable Add, and it
/// would be WRONGLY suppressed (its own IntMul dropped, even though the
/// call still needs mul's real computed value).
#[test]
fn lea_synthesis_libm_call_argument_use_prevents_suppression() {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let base_v = b.emit(
        entry,
        Inst::Param {
            index: 0,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let idx = b.emit(
        entry,
        Inst::Param {
            index: 1,
            ty: Ty::I64,
        },
        Ty::I64,
        dummy_span(),
    );
    let four = b.emit(entry, Inst::ConstI64(4), Ty::I64, dummy_span());
    let mul = b.emit(entry, Inst::Mul(idx, four), Ty::I64, dummy_span());
    let _add = b.emit(entry, Inst::Add(mul, base_v), Ty::I64, dummy_span());
    // `mul` is ALSO used as a libm call argument -- an escaping use (not
    // an Add-fusion pattern), so it must NOT be suppressed even though
    // the Add above fuses it. (Using an I64 value as a libm call arg
    // isn't realistic front-end output -- real calls are f64-only per
    // the type checker -- but this is a raw IR-construction unit test
    // exercising the selector's suppression logic directly, the same
    // way lea_synthesis_escaping_use_prevents_suppression's sibling
    // tests do; MachineInst::CallLibm's selection doesn't inspect arg
    // types, only uses_of's traversal, which is what's under test here.)
    let call_r = b.emit(
        entry,
        Inst::Call {
            func: forge_ir::LibFunc::Sin,
            args: smallvec::smallvec![mul],
        },
        Ty::F64,
        dummy_span(),
    );
    b.f.blocks[entry.0 as usize].term = Some(Terminator::Return(call_r));

    let selected = select(&b.f);

    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::Lea { .. })));
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::IntMul { dst, .. } if *dst == mul)));
    assert!(selected
        .insts
        .iter()
        .any(|i| matches!(i, MachineInst::CallLibm { args, .. } if args.as_slice() == [mul])));
}
```

- [ ] **Step 8: Run the tests and confirm they pass**

Run: `cargo test -p forge-x64 --lib 2>&1 | tail -60`
Expected: `lea_synthesis_libm_call_argument_use_prevents_suppression` passes alongside everything else.

- [ ] **Step 9: Run the FULL workspace test suite to confirm no regressions**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 10: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 11: Commit**

```bash
git add crates/forge-x64/src/machine_inst/mod.rs crates/forge-x64/src/machine_inst/tests.rs crates/forge-x64/Cargo.toml
git commit -m "feat(forge-x64): MachineInst::CallLibm, real libm call selection"
```

## Context for Task 2

Every golden `Vec<MachineInst>` assertion above follows the exact style already established throughout `machine_inst/tests.rs` (e.g. `select_lowers_a_single_i64_constant_and_return`). `Cargo.lock` MAY need updating as a side effect of the `Cargo.toml` change in Step 1 (in practice it doesn't, since `smallvec` is already resolved workspace-wide via `forge-ir`'s existing dependency on it) — this happens automatically on the next `cargo build`/`cargo test` if it's needed at all, no manual step required, but include the updated `Cargo.lock` in the Step 11 commit if `git status` shows it as modified.

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

Confirm exit criteria 1-7 from the design doc are met (see `docs/superpowers/specs/2026-08-09-phase-7e-libm-call-design.md`'s "Exit criteria" section). Criterion 8 (CHECKLIST.md's remaining Phase 7 bullets getting accurate `**note (Phase 7e):**` annotations distinguishing what this slice delivers from what's deferred to the new "final code-emission pipeline" task) is NOT this task's job — per this project's established convention (used identically for 7a-7d), CHECKLIST.md annotations are added by the separate final-holistic-review pass that runs after this plan's tasks complete, left uncommitted for review, not baked into the implementation plan itself.
