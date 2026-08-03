# Design: forge Phase 0-3 slice — bootstrap, parser, SSA IR, interpreter

**Status:** Approved for planning
**Scope:** CHECKLIST.md Phase 0 (Bootstrap), Phase 1 (Frontend), Phase 2 (SSA IR), Phase 3 (Interpreter/oracle) — scalar language only.
**Out of scope:** codegen (Phase 5-9), optimizer (Phase 4), array/`@vectorize` mode, CLI, workbench, full differential-testing infra (Phase 11).

This is the first implementation slice of the `forge` JIT compiler described in
`PROMPT.md`/`SPEC.md`/`CHECKLIST.md`. Those three files are the source of
truth for the whole project; this doc scopes and resolves the ambiguities
found while starting on the first slice of it. The relevant ambiguities were
fixed directly in `SPEC.md`, `CHECKLIST.md`, and `PROMPT.md` as part of this
design (see "Resolved ambiguities" below) — this doc records *why*, and adds
the session-scoping decisions (what's in vs. deferred) that don't belong in
the permanent spec files.

## Goal

Produce a real, tested `source string → tokens → AST → typed AST → SSA IR →
interpreted result` pipeline, with nothing faked or stubbed along that path.
This becomes the correctness oracle (PROMPT.md rule #1) that every later
codegen phase is differential-tested against. Also validate on day one that
this machine can actually run JIT'd code at all (PROMPT.md rule #3), since if
that fails nothing downstream matters.

## Workspace structure

Virtual workspace, all 13 crates from SPEC §17 created now as real `Cargo.toml`
members (most as empty-lib stubs) so later phases don't need workspace
surgery:

```
forge/
├── Cargo.toml
├── entitlements.plist        # com.apple.security.cs.allow-jit
├── Makefile                  # test, codesign
├── .github/workflows/ci.yml
├── crates/
│   ├── forge-syntax/         # ACTIVE — lexer, Pratt parser, AST, typecheck
│   ├── forge-ir/             # ACTIVE — SSA IR, builder, verifier, RtValue, interpreter, printer
│   ├── forge-mem/            # examples/spike.rs only; ExecutableBuffer type is Phase 5
│   ├── forge-opt/            # stub
│   ├── forge-regalloc/       # stub
│   ├── forge-x64/            # stub
│   ├── forge-aarch64/        # stub
│   ├── forge-wasm/           # stub
│   ├── forge-runtime/        # stub
│   ├── forge-simd/           # stub
│   ├── forge-bench/          # stub
│   ├── forge-cli/            # stub
│   └── forge-wasm-api/       # stub
```

Dependencies added at workspace level per PROMPT.md Phase 0: `libc nix region
raw-cpuid smallvec bitvec rustc-hash thiserror anyhow` (runtime), `iced-x86
capstone criterion proptest` (dev — `iced-x86`/`capstone` unused until Phase 6/9,
but added now so `cargo add` isn't repeated later).

## Day-one spike

`crates/forge-mem/examples/spike.rs`, verbatim per PROMPT.md Phase 0: raw
`libc::mmap` → write `48 89 F8 C3` → `mprotect` → `transmute` → call. This is
deliberately **not** built through any `ExecutableBuffer` abstraction — that
type is Phase 5's job. Both the generic-Unix `mmap`/`mprotect` path and the
macOS-arm64 `MAP_JIT` path are written behind `cfg`, since whichever this
machine is, the other path will be needed by Phase 5 anyway and getting the
split right now costs nothing extra.

Needs `entitlements.plist` + a `make codesign` step (codesigns
`target/debug/examples/spike`) to actually get an executable page on macOS.

## CI

One GitHub Actions job, macOS-latest: `cargo build --workspace`, `cargo test
--workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
plus codesign-then-run the spike example. The full four-leg matrix (Linux x64,
aarch64-linux under QEMU, wasm32) from CHECKLIST Phase 0 is deferred until
Phase 9/14 actually produce something for those legs to build — no point
running CI legs with nothing to test.

## Language & type system (scalar subset)

- **Lexer/parser** (`forge-syntax`): hand-written lexer producing
  `(Vec<Token>, Vec<Diagnostic>)` (never `Result`); Pratt parser over the
  precedence table now documented in SPEC §3 "Operators & precedence".
- **No array/`@vectorize` grammar** in this slice — SIMD is Phase 10 and
  nothing would consume `Load`/`Store`/indexing yet. Add it when Phase 10
  needs it (YAGNI).
- **Type checker**: `f64`/`i64`/`bool`, implicit `i64 → f64` widening.
  Free identifiers become `params` in first-appearance order; a param's type
  is inferred from its use sites (unify across every place it appears —
  e.g. an operand of `&`/`<<` forces `i64`; an operand of `sqrt` or mixed
  arithmetic with an `f64` forces `f64`), erroring with both use-site spans on
  conflict. This resolves an underspecified corner of SPEC §3/CHECKLIST
  Phase 1 — the language grammar has no type-annotation syntax, so inference
  from usage is the only option.
- **AST**: arena (`Idx<Expr>`) with parallel `spans`.

## SSA IR (`forge-ir`)

- `Inst` enum restricted to the instructions this slice can actually
  construct: constants, `Param`, arithmetic (`Add/Sub/Mul/Div/Rem/Neg`),
  bitwise/shift (`And/Or/Xor/Not/Shl/Shr/Sar`), `Cmp`, `Phi`, the
  single-instruction intrinsics (`Sqrt/Abs/Min/Max/Floor/Ceil/Round/Trunc`),
  `Fma` (parseable directly via the `fma()` intrinsic — see "Resolved
  ambiguities"), `Call` for libm (`sin/cos/tan/exp/log/pow`), `IToF/FToI`.
  The full ~40-variant enum from SPEC §5 is defined, but `Select`/SIMD/
  `Load`/`Store` variants stay unconstructed until their phases.
- Braun et al. SSA construction (`read_variable`/`write_variable`/
  `read_variable_recursive`) with incomplete-φ handling and trivial-φ
  removal, per SPEC §5.1 — needed even without loops, because it's what makes
  `if` correct without a full dominance-frontier computation.
- IR verifier (single-def, dominated uses, φ arity == pred count, per-opcode
  type consistency), run after construction. This is the same verifier every
  later optimizer pass reuses in debug builds.
- Textual IR printer (tests assert on IR shape, e.g. `sqrt(x*x+y*y)` == 6
  instructions).

## Interpreter (the oracle)

`RtValue` (`F64(f64) | I64(i64) | Bool(bool)`) + `interpret(f: &Function,
args: &[RtValue]) -> RtValue`, per the now-updated PROMPT.md Phase 3 code
sample. IEEE-754 semantics exact (NaN propagation, ±0, ±Inf, subnormals, no
shortcuts); integer ops use Rust's wrapping arithmetic, matching how the JIT's
raw `add`/`sub`/`imul` will behave later.

## Resolved ambiguities (also fixed directly in SPEC.md / CHECKLIST.md / PROMPT.md)

1. **Typed parameters need typed values, not `&[f64]`.** PROMPT.md's
   illustrative `interpret(f, args: &[f64]) -> f64` can't represent `i64`/
   `bool` params, but SPEC §5's `Function.params: Vec<(Symbol, Ty)>` allows
   them. Introduced `RtValue` (distinct name from the IR's own `Value(u32)`
   index type) as the real argument/result representation everywhere,
   including the eventual JIT calling convention (`CompiledExpr::call`).
2. **`fma` is both a parseable intrinsic and an optimizer output.** SPEC §3
   lists `fma` as a user-callable intrinsic; SPEC §5's original comment on
   `Inst::Fma` said "created by FMA contraction, never by the parser" —
   contradiction. Resolved: both are legitimate origins of the same
   instruction; codegen doesn't need to know which produced it.
3. **Bitwise/shift operators were missing from the token list.** SPEC §3's
   integer-domain example (`(n * 2654435761) >> 16`) uses `>>`, and SPEC §5's
   `Inst` enum has `And/Or/Xor/Not/Shl/Shr/Sar`, but CHECKLIST Phase 1's
   `TokenKind` bullet never listed `& | ^ << >>`, and there was no token for
   bitwise-not distinct from logical `!`. Added `& | ^ << >> ~` to the token
   set and a full precedence table to SPEC §3.

## Testing plan

**Unit tests** (CHECKLIST Phase 1-3): precedence/associativity/unary-binding;
type error on `1 + true`; `if`-branch type mismatch; intrinsic arity
mismatch; `sqrt(x*x+y*y)` → exactly 6 IR instructions; `if` → 4 blocks with a
correct φ; verifier rejects a hand-built use-before-def and a malformed φ;
known-value tests for every intrinsic; NaN/Inf propagation through every
arithmetic op; `if` with a NaN comparison takes the else branch; i64
wrapping-overflow test (now meaningful with real `i64` params).

**Property test (light):** `parse(print(ast)) == ast` round-trip only. The
full `arb_expression`/differential-testing infra (Phase 11) is **not** built
yet — there's no JIT to differential-test against, so building it now would
be scaffolding with nothing to verify. Left as a `// TODO(phase 11)` marker.

## Exit criteria

1. `cargo run --example spike -p forge-mem` prints `JIT works: f(42) = 42` on
   this machine (codesigned).
2. `cargo test --workspace` passes, covering all unit tests above.
3. An integration test in `forge-ir/tests/` does the real end-to-end path:
   source string → lex → parse → typecheck → lower to SSA → `interpret()` →
   compare against a hand-computed `RtValue`, for representative expressions
   (straight-line arithmetic, `if`/`let`, an intrinsic, a libm call, an
   i64/bitwise expression, a deliberately NaN-producing one).
4. `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
5. CI green on the basic macOS-arm64 workflow.
