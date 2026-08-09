# Design: forge Phase 7e — libm Call Selection & Address Resolution

**Status:** Approved for planning
**Scope:** The fifth sub-slice of CHECKLIST.md Phase 7. Two concrete, real, testable-now deliverables: (1) `MachineInst::CallLibm`, replacing `select_inst`'s `Inst::Call => unimplemented!(...)` panic with a real (still virtual-register, SSA-form) selection arm, mirroring the pattern every other Phase 7a-7d arm already established; (2) `libm_address(func: LibFunc) -> i64`, real FFI resolution of `Sin`/`Cos`/`Tan`/`Exp`/`Log`/`Pow`'s absolute addresses, in a new `crates/forge-x64/src/libm.rs`.
**Out of scope (deferred to a new task, not yet started — "Final code-emission pipeline"):** the actual byte-level call sequence CHECKLIST's remaining Phase 7 bullets describe — spilling live caller-saved XMM registers, marshalling args into `xmm0`/`xmm1` per the ABI, aligning `rsp` at the call site, emitting `mov_reg_imm` + `call_reg`, moving the `f64` result out of `xmm0`. All of that needs real `PhysReg` assignments and real liveness information that only exist once Phase 8's register allocator runs — see "Why this is smaller than CHECKLIST's bullets suggest" below. **Win64 entirely** — deferred, same reasoning as 7d.

## Why this is smaller than CHECKLIST's bullets suggest

CHECKLIST.md's remaining Phase 7 bullets (`libm call sequence: spill live caller-saved registers, align, call, restore`; `argument marshalling per ABI`; `return value in xmm0`; the four integration tests) describe a single, real, byte-emitting call sequence — not a virtual-register `MachineInst`. But every one of those bullets needs information that doesn't exist yet in this project's own build order:

- **"Spill live caller-saved registers"** needs to know which registers are *live* across the call site — that's liveness analysis, Phase 8's job, not instruction selection's.
- **"Marshal args into xmm0/xmm1"** needs to know which *real* XMM register an SSA `Value` is assigned to, so the marshalling code can decide whether a `movapd` is even needed (it isn't, if the allocator already happened to place the argument in `xmm0`) — that's Phase 8's allocation output.
- **"Align at the call site"** needs to know the exact runtime `rsp` offset at that point in the function, which depends on how much spill space Phase 8 requests and where in program order this call falls relative to other spills — not decidable during instruction selection, which doesn't know registers or spill slots yet.
- **The four integration tests** (`extern "C"`-callable, callee-saved preserved, alignment holds, `sin`+`cos` correct) all require a *fully emitted, runnable* machine-code function — impossible before real bytes exist, which (per every Phase 7a-7d design doc's established resolution to the Phase 7/8 circular dependency) doesn't happen until a final MachineInst-to-bytes emission step runs, and that step needs Phase 8's real register assignments as its input.

This isn't a new problem Phase 7e invented — it's the exact same shape as 7c's constant pool, which built the *data structure* (dedup, `PoolIndex`) while deferring the *byte emission* (RIP-relative loads) to the same future step. Phase 7e does the same thing for calls: build the *selection-level* representation (which function, which SSA args, where the result goes) now, defer the *emission-level* sequence (spill/marshal/align/call/restore) to later. A new task ("Final code-emission pipeline: MachineInst → real Assembler bytes") has been added to this project's tracked work specifically to own that later step — it is blocked on Phase 8's sub-slices landing first, and in turn blocks the final Phase 7+8 holistic review. That task, not this one, is what will actually let CHECKLIST's remaining libm bullets and their four integration tests be marked done.

## Architecture

### 1. `MachineInst::CallLibm`

Added to the `MachineInst` enum in `crates/forge-x64/src/machine_inst/mod.rs`, immediately after `FloatToInt` (grouped with the "Conversions" section is wrong — it gets its own one-line section comment, `// libm calls`, placed after Conversions and before "Control flow", matching how `Lea` got its own section between integer and float arithmetic in 7b):

```rust
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
```

`select_inst`'s `Inst::Call` arm changes from the current panic to:

```rust
Inst::Call { func, args } => {
    self.insts.push(MachineInst::CallLibm { dst, func: *func, args: args.clone() });
}
```

This is a direct, mechanical translation — `forge_ir::Inst::Call` already carries exactly `{ func: LibFunc, args: SmallVec<[Value; 2]> }` (confirmed by reading `forge-ir/src/ir.rs`), so `MachineInst::CallLibm` copies that shape verbatim rather than inventing a new one. `LibFunc` is a closed, 6-variant enum (`Sin, Cos, Tan, Exp, Log, Pow`) — five unary, one binary (`Pow`) — `args`' `SmallVec<[Value; 2]>` sizing already matches the maximum arity with zero heap allocation for any real call.

**Not participating in `compute_coalescing_hints`**: a call's `dst` isn't a 2-address-destructive x86 operation — its real location is wherever the ABI return convention places it (`xmm0`), unrelated to any operand's register. `CallLibm` simply isn't matched by that function's existing arms, so it falls through its `_ => {}` catch-all with no code change needed there.

**Not minting any synthetic `Value`**: unlike `Fma`'s `mul_tmp`, `dst` here is always a real IR value (the typed result of the original `sin(x)`/`pow(x,y)` expression) — no `self.fresh(...)` call needed.

**No change needed to `find_fully_fusable_scaled_indices`'s liveness counting**: it already walks every `Inst` via `forge_ir::uses_of`, which already has a `Inst::Call { args, .. } => args.iter().copied().collect()` arm (confirmed by reading `forge-ir/src/ir.rs`) — a `Value` used only as a libm call argument is already correctly counted as "used," so it's never wrongly suppressed by the lea-fusion pre-pass.

### 2. `libm_address` — real address resolution

New file `crates/forge-x64/src/libm.rs`:

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

Exported from `lib.rs` as `pub use libm::libm_address;`.

**Why this is real, useful work today, not premature**: unlike the call *sequence*, resolving a symbol's address has nothing to do with register allocation — it's a pure FFI lookup, fully testable in isolation right now: cast the returned `i64` back to the appropriate function-pointer type, call it, and compare against Rust's own `f64::sin()`/`.cos()`/etc (which on every platform this project's CI targets — `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu` — is itself backed by the system's libm, so the two really are the same underlying implementation, not merely "close"). This gives the future emission step a real, correct, already-tested building block to call into, rather than another `unimplemented!()` to fill in later.

**Why `extern "C"` declarations, not a `libm` crate dependency**: this project has no `libm` crate dependency anywhere (confirmed by searching the whole workspace), and `forge-opt/src/strength.rs` already established the precedent of a raw `extern "C"` FFI declaration to `pow` for exactly this reason — the system's libm is what's actually being called into at runtime regardless, so declaring the symbols directly is simpler than adding a crate whose own implementation might not even be what gets linked. `strength.rs`'s existing comment block (documenting why `pow`'s address needs to be routed through an opaque `black_box`'d function pointer to defeat LLVM's `LibCallSimplifier` compile-time rewriting) is about a *different* concern — that code calls `pow` as a compile-time *test oracle* baked into the compiled `forge-opt` binary; `libm.rs`'s `libm_address` instead captures the *runtime address* of the same symbol for the JIT to call into *at JIT-execution time*, a genuinely different use, so the two pieces of code don't need to share implementation, only the underlying insight that these are real libc symbols reachable via `extern "C"`.

**Not scoped to build**: any actual dynamic symbol resolution beyond what `extern "C"` + the system linker already does (e.g. `dlsym`, `GetProcAddress`) — none of that is needed since these are ordinary libc symbols the whole process already links against at compile time; "resolution" here just means "take the address of an already-linked function."

## New dependency

`crates/forge-x64/Cargo.toml` currently depends on `forge-ir` only (`smallvec` is a dev-dependency, used by `machine_inst`'s tests, but not a regular one). `MachineInst::CallLibm`'s `args` field needs `smallvec::SmallVec` as a real (non-dev) type, so `smallvec.workspace = true` moves from `[dev-dependencies]` to `[dependencies]` (it stays listed in `[dev-dependencies]` too only if still separately needed there — check whether removing it from dev-deps and only listing it under `[dependencies]` still satisfies existing test code, since a regular dependency is automatically visible to a crate's own `#[cfg(test)]` code; likely `[dev-dependencies]`'s `smallvec.workspace = true` line becomes redundant and should be removed, not duplicated).

## Testing

**Existing test that must be replaced, not just left alone**: `machine_inst/tests.rs` currently has `select_panics_on_call_with_a_clear_deferral_message`, a `#[should_panic(expected = "Phase 7e")]` test asserting `Inst::Call` still panics — a direct consequence of 7a-7d's `unimplemented!("libm call lowering ships in Phase 7e")` message. Once this slice replaces that panic with a real `CallLibm` push, this test's premise is gone and it must be deleted and replaced with a real assertion on the produced `MachineInst` (folded into the two golden-`Vec<MachineInst>` tests below) — otherwise the suite ships with a guaranteed-failing test.

- `libm_address` returns a distinct, real, callable address for at least one representative unary function (`Sin`) and the one binary function (`Pow`) — cast back through the same `Unary`/`Binary` function-pointer types used internally, call with representative inputs, compare against `f64::sin()`/`f64::powf()` for **bit-exact** equality (not approximate — same underlying libm implementation, so exact equality is the correct, achievable bar, matching this project's existing FMA-vs-approximation precision discipline of never silently accepting approximate behavior where exact is achievable). **Hazard, already hit once by this codebase**: `crates/forge-opt/src/strength.rs`'s own comments (around its `libm_pow` test oracle) document that at `--release`, LLVM's `LibCallSimplifier` recognizes calls literally named `pow`/`sin`/etc — even through a hand-written `extern "C"` declaration, not just `f64::powf` — and rewrites special-cased exponents/inputs (e.g. `pow(x, 2.0)`, `pow(x, -1.0)`, `pow(x, 0.5)`) to `fmul`/`fdiv`/`sqrt` at compile time, which would make comparing against `f64::powf` for exactly those inputs circular (both sides silently become the same rewritten expression, not independent implementations). Avoid this two ways: (1) don't pick test inputs matching a known special-cased identity — the representative inputs already chosen for this task (`{0.5, 1.0, 2.0, -1.5}` unary, `(2.0, 10.0)` for `Pow`) aren't special-cased and are fine as-is; (2) route the `f64::*` oracle side through `std::hint::black_box` as well, the same fix `strength.rs` already applies to its own oracle, for defense in depth against a future test-writer adding a special-cased input without realizing the risk.
- `libm_address` returns six pairwise-distinct addresses (one assertion, all six `LibFunc` variants) — guards against a copy-paste mistake where two arms accidentally return the same extern symbol's address.
- `select_inst`'s new `Inst::Call` arm: golden-`Vec<MachineInst>`-style tests (matching every other `machine_inst/tests.rs` test), one for a unary call (`sin(x)` → `CallLibm { dst, func: LibFunc::Sin, args: [x] }`) and one for the binary case (`pow(x, y)` → `CallLibm { dst, func: LibFunc::Pow, args: [x, y] }`), confirming the exact `MachineInst` shape produced, not just "doesn't panic."
- A coalescing-hints regression test: `compute_coalescing_hints` on a `CallLibm`-containing instruction sequence produces no hint entry for the call's `dst` (guards against a future accidental match-arm addition reintroducing an incorrect hint).
- A fusable-scaled-index regression test: a `Value` used only as a libm call argument is correctly excluded from `find_fully_fusable_scaled_indices`'s suppression set (i.e., confirms `uses_of`'s existing `Inst::Call` coverage is actually exercised through this new arm, not just theoretically present in `forge-ir`).

## Exit criteria

1. `MachineInst::CallLibm { dst, func, args }` exists, matching `forge_ir::Inst::Call`'s shape exactly.
2. `select_inst`'s `Inst::Call` arm produces a real `CallLibm` MachineInst instead of panicking; the exhaustive-match discipline (no wildcard arm) is preserved.
3. `libm_address(func: LibFunc) -> i64` exists in `crates/forge-x64/src/libm.rs`, exported from `lib.rs`, correctly resolving all 6 `LibFunc` variants to real, distinct, callable libc symbol addresses.
4. `CallLibm` does not produce a coalescing hint and does not get wrongly suppressed by the lea-fusion pre-pass.
5. Tests cover: `libm_address`'s bit-exact correctness against Rust's own math functions (at least `Sin` and `Pow`), all-six-addresses-distinct, `select_inst`'s new arm's exact `MachineInst` output for both unary and binary calls, the coalescing-hints non-participation, and the fusable-scaled-index non-suppression.
5a. The now-obsolete `select_panics_on_call_with_a_clear_deferral_message` test is removed and replaced by real `CallLibm`-shape assertions (see "Testing" section) — the suite has no test left asserting `Inst::Call` panics.
6. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
7. No regressions in any Phase 6/7a-7d `forge-x64` test or any other crate's tests.
8. CHECKLIST.md's remaining Phase 7 bullets get accurate `**note (Phase 7e):**` annotations distinguishing what this slice actually delivers (selection + address resolution) from what's explicitly still deferred to the new "final code-emission pipeline" task (the real spill/marshal/align/call/restore byte sequence and all four integration tests) — so nothing here gets silently miscounted as done.
