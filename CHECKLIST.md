# CHECKLIST.md — `forge`: A JIT Compiler for Expression Evaluation

> Priority: 🔴 blocking · 🟡 important · 🟢 enhancement · 🔵 stretch
> **Differential testing (Phase 11) is the spine. A JIT that computes wrong answers silently is worse than no JIT. Wire up interpreter-vs-JIT comparison in Phase 6, the moment the first instruction executes.**
> **Every encoder function gets a disassembler round-trip test in the same commit. No exceptions.**

## Current implementation status — 2026-09-12

This section is the current-state source of truth for the implementation
audit. The long phase sections below retain the original design history and
scope notes; their historical `[ ]` markers are not a claim that already
implemented lower-level work is absent.

| Area | Current state | Evidence |
|---|---|---|
| Front end, SSA IR, interpreter, optimizer, executable memory, x86 encoder and selection | Implemented and workspace-tested | `crates/forge-*/src`, `cargo test --workspace --offline` |
| Final x86 emission | Implemented for selected scalar instructions, System V and Win64 ABI parameters (including stack-backed parameters for both ABIs), libm calls including exact f64 remainder through process-local `fmod`, spill reload/store, stack frames, control flow, return placement, ABI-safe scratch selection, and typed native calls through the runtime trampoline; broader external-ABI coverage remains open | `crates/forge-emit`, `crates/forge-runtime`, focused emitter tests, Windows-native CI |
| Fixed-register allocation conflict | Implemented: non-fixed active victims spill; overlapping fixed intervals fail explicitly | `forge-regalloc` linear-scan tests |
| Variable shifts | Implemented with allocator constraints for RCX/CL, an emission fallback move, and preservation of unrelated live RCX values | `forge-regalloc::excluded_registers`, emitter/layout tests |
| Float remainder | Implemented as an exact process-local `fmod` call for native x86-64 and AArch64 emission, with constant folding and runtime bit checks; WASM artifacts now emit a typed `forge.fmod(f64, f64) -> f64` host import when `%` is present, while modules without remainder remain dependency-free | `forge-x64/src/machine_inst/mod.rs`, `forge-aarch64/src/lib.rs`, `crates/forge-wasm`, `forge-runtime` |
| Runtime | Implemented source lowering, optimization, selection, allocation, verification, native x86-64 JIT, native AArch64 execution for supported scalar subsets, typed x86-64 execution for f64/i64/bool signatures through System V and Win64 register/stack-aware trampolines, typed AArch64 execution for supported AAPCS64 f64/i64/bool signatures including stack-backed values, interpreter fallback for unsupported targets/operations, and thread-safe interpreter → baseline → optimized tier promotion; native execution is differentially checked against the interpreter with generated expressions and IEEE special values, while the real Node WebAssembly runtime executes the same generated module for all-f64 cross-backend cases plus typed i64/bool artifact cases | `crates/forge-runtime`, `crates/forge-runtime/tests/cross_backend.rs` |
| Verification stress coverage | Implemented deterministic 500-live-value native spill-frame coverage, randomized valid-IR optimizer/verifier property coverage with 512 generated programs, and a reproducible 100,000-expression native differential corpus covering arithmetic, guarded division, abs/sqrt, min/max, conditionals, FMA, NaNs, infinities, signed zero, and subnormals; Linux CI runs the executable-memory no-leak test under Valgrind Memcheck, while the macOS Miri limitation remains documented | `crates/forge-emit/tests/execution_corpus.rs`, `crates/forge-opt/tests/differential.rs`, `.github/workflows/ci.yml` |
| CLI | Implemented documented `eval`, `compile`, `asm`, `ir`, `cfg`, `regalloc`, `bench`, `verify`, `cpuinfo`, and `repl` command surface; the REPL has session bindings/history/inspection commands, terminal-aware color with `NO_COLOR`, AArch64 emission, and tested exit-code classification; `asm --annotate` reports live allocated locations, while `bench` supports warmups, reusable compiled-call timing, and stable JSON reports | `crates/forge-cli` |
| SIMD | Runtime `CpuFeatures` snapshot, deterministic f64 width selection, explicit host-safe feature masking, explicit typed vector loop plans with induction start/step, full-chunk and tail blocks, vector loads/stores, packed straight-line and pure structured-control-flow f64 array execution on SSE2/SSE4.1/AVX2/AVX-512F/NEON, lane-mask propagation with predicated selects, exact AVX2+FMA and AVX-512F `fma` evaluation when FMA is detected, exact packed `min`/`max` with Forge NaN and signed-zero semantics, packed floor/ceil/trunc where the selected ISA provides exact rounding instructions, exact ties-away-from-zero packed `round` on SSE4.1/AVX2/AVX-512F, scalar epilogues, AVX-512F k-masked zeroing tails, source-ordered `reduce_sum` over packed chunks, scalar f64 and typed i64 broadcast parameters, constant positive/negative indexed load offsets, repeated source columns at distinct constant offsets, packed unary/binary libm calls through an exact lane-preserving adapter, and exact scalar fallback are implemented. The documented source-level `@vectorize output[index] = expression` form now parses indexed f64 columns, lowers to verified array loop/memory IR with induction, offset-bearing `Load`, and `Store`, and executes through the same packed/tail evaluator; canonical and constant-offset `column[index ± constant]` addressing, loop-invariant dynamic scalar offsets through the typed broadcast API, and unindexed f64 broadcasts are supported end to end. Nested source loops now accept row-major flattened columns with explicit dimensions and checked scalar evaluation for arbitrary nested addressing; stable EVEX packed-double byte forms remain separately covered | `crates/forge-simd`, `crates/forge-ir/src/array.rs`, `crates/forge-syntax/src/array.rs`, `crates/forge-x64/src/assembler.rs` |
| AArch64 | Native target capability API plus tested scalar integer/float/conversion/memory/branch/immediate encoder forms; AAPCS64 scalar f64 expression emission includes arithmetic, NaN-compatible min/max, comparisons, CFG branches, typed phi edge copies, aligned literal pools, CLI output, native ARM execution, and Linux ARM64 QEMU coverage; mixed scalar i64/f64/bool parameter banks, integer operations, scalar comparisons, and i64↔f64 conversions are emitted for f64-result functions, with integer negation using the architectural XZR register; temporaries use caller-saved D16..D31/X8..X18 first, then preserve allocated D8..D15/X19..X28 in aligned save/restore frames; every generated non-leaf/spill frame now uses checked AAPCS64 `stp x29, x30, [sp, #-16]!` / `ldp x29, x30, [sp], #16` frame records, and incoming stack arguments account for that record; explicit budget rejection and native f64/i64 frame execution coverage remain; straight-line i64 expressions beyond the register budget spill to aligned stack slots and reload through preserved X28..X30 scratch registers, while high-pressure all-f64 straight-line and structured CFG expressions spill to aligned stack slots and reload through caller-saved D29..D31 scratch registers, including f64 phi edge stores; high-pressure mixed f64/i64/bool expressions returning f64 spill typed values to aligned stack slots across straight-line and structured CFG expressions, including f64 and integer phi edge stores, while preserving the integer scratch frame and both AAPCS64 parameter banks; scalar f64 libm calls use process-local absolute symbol dispatch through AAPCS64 D0/D1 arguments, aligned frames, and saved link registers, including exact f64 remainder through `fmod`, and mixed f64/i64/bool parameter functions spill both incoming AAPCS64 banks across those calls; native FRINTM/FRINTP/FRINTA/FRINTZ lowering now covers floor, ceil, Rust-compatible ties-away-from-zero round, and trunc in register and stack-spill paths; the runtime now invokes supported mixed f64/i64/bool-to-f64, typed bool, and straight-line i64 signatures through an AAPCS64 typed trampoline, including overflow values in either bank through aligned stack argument slots; comparison values are materialized with canonical CSET 0/1 words for returned, composed, and stack-backed bool results; the straight-line all-i64 emitter and typed trampoline also marshal integer values beyond X0..X7 from aligned incoming stack slots; a CFG-aware interference-coloring allocator now models block-entry phi definitions, per-edge phi uses and edge-live values, reuses non-interfering D/X temporaries across structured control flow, emits parallel phi moves with reserved cycle-break scratch registers, and preserves selected callee-saved registers; arbitrary mixed-signature external calls remain open | `crates/forge-aarch64`, `crates/forge-runtime` |
| WASM | Tested typed scalar byte emitter for `f64`, `i64`, and bool values, including arithmetic, comparisons, conditionals, lets, min/max, fma, and f64 remainder lowering; remainder artifacts declare the exact typed `forge.fmod` host import and execute in real Node/WebAssembly with bit checks, while ordinary modules remain dependency-free; structured artifact JSON exposes bytes, hex, parameter types, result type, required imports, lowered/optimized IR, CFG, decoded stack-machine instructions with byte offsets and stack depth, and logical stack-value lifetime intervals; parse/type diagnostics now include source spans and a serialized AST; `benchmark(source, sizes)` exposes portable baseline timings and results; reproducible `wasm-pack --target web --release` plus `wasm-opt -Oz` packaging is CI-validated under the documented gzip-size boundary; native register intervals remain inapplicable because this target is stack-machine WASM | `crates/forge-wasm`, `crates/forge-runtime/tests/cross_backend.rs`, `crates/forge-wasm-api`, `workbench/src/compiler.ts`, `workbench/src/App.tsx`, `.github/workflows/ci-containers.yml`, `containers/Dockerfile.wasm-ci` |
| Benchmarks | Reusable compiled-expression benchmark helper exists; allocator Criterion benchmark now measures about 42–43 µs for its 1000-value workload, below the 50 µs target on the validation machine | `crates/forge-bench`, `crates/forge-regalloc/benches` |
| Workbench | React/Vite/TypeScript workbench now provides CodeMirror expression editing with syntax highlighting and source-span squiggles, 200 ms debounced compilation, shared Zustand artifact state, D3 AST and dagre CFG views, IR stepper/diff, target-aware native interval/assembly panels, WASM bytes, Recharts benchmark samples, tier labels, and x86-64/AArch64/WASM plus scalar/array selectors; WASM is executable in the browser, while x86-64/AArch64 are serialized inspection artifacts and native bytes are never executed in the browser | `workbench/`, `crates/forge-wasm-api`, `containers/Dockerfile.workbench` |
| Windows executable memory | Implemented `VirtualAlloc`/`VirtualProtect`/`FlushInstructionCache`/`VirtualFree` backend; Win64 register/stack parameters, shadow space, caller-preserved scratch, live-value preservation across libm, register- and stack-backed typed runtime calls, allocation of RBX/RSI/RDI/R12-R15 and XMM6-XMM15, aligned nonvolatile save/restore frames, spill-slot disjointness, and native `windows-latest` high-pressure coverage are implemented; broader external-ABI coverage remains open | `crates/forge-mem`, `crates/forge-emit`, `crates/forge-regalloc`, `crates/forge-runtime`, `.github/workflows/ci.yml` |

The remaining open rows are intentional scope boundaries, not silent stubs.

Issue #434 implements the canonical source-level array boundary end to end,
Issue #448 extends it with constant element offsets, and Issue #452 allows
one source column to be loaded at multiple constant offsets. The evaluator uses
the maximal valid shared input window, so negative offsets establish a checked
input origin and positive offsets trim the output tail; short windows produce
an empty result rather than an unchecked read. The grammar accepts one row
loop with `column[index ± integer]` loads and unindexed f64 broadcasts, not
dynamic pointer arithmetic or nested source loops. Every indexed occurrence is
represented as its own verified load, so stencil-style expressions may reuse a
column at several offsets. Unsupported packed operations still use the
verified scalar element fallback.

Issue #444 completes scalar broadcasts for this boundary. An unindexed f64
identifier such as `scale` in `@vectorize result[i] = a[i] + scale` is retained
as a scalar-broadcast parameter in the verified array IR and splatted directly
by packed backends. `evaluate_vectorized_with_broadcasts` and its feature-masked
variant supply broadcasts separately from columns; differential tests cover
all lengths 1 through 100 and the exact scalar fallback.

Issue #456 completes packed libm calls for this boundary. Unary `sin`, `cos`,
`tan`, `exp`, and `log`, plus binary `pow`, lower into typed vector call nodes.
The packed backend applies the same scalar Rust/libm operation independently
to each active lane and repacks the results, so surrounding arithmetic and
structured control flow remain packed while Forge's NaN, infinity, signed-zero,
and platform-libm behavior stays identical to the interpreter oracle. The
adapter is also used by the AVX-512 masked-tail path; differential tests cover
full chunks, tails, and scalar fallback.

Issue #460 completes loop-invariant dynamic scalar offsets. An indexed load
such as `a[i + shift]` may use an i64 free parameter, supplied through
`evaluate_vectorized_with_typed_broadcasts` as an `RtValue::I64`. The runtime
materializes that call-time value into the existing checked constant-offset
window, preserving packed contiguous loads, scalar epilogues, exact fallback,
and the same empty-window behavior for out-of-range shifts. The legacy f64-only
entry point continues to reject a source that requires a typed i64 broadcast so
callers cannot accidentally pass a value with the wrong ABI.

Issue #464 completes the WASM inspection artifact boundary. The browser-facing
artifact now decodes the emitted function body into exact byte-offset stack
instructions, reports abstract stack depth before and after each instruction,
and exposes logical stack-value lifetime intervals. The Workbench renders
those artifacts as stack instructions and stack lifetimes; native register
intervals remain intentionally inapplicable to the stack-machine target.

Issue #468 completes the nested source-loop boundary. The extended
`@vectorize output[i, j, ...] = expression` declaration uses row-major
flattened f64 columns and caller-supplied dimensions. The nested evaluator
checks shape products, broadcast types, and every computed index before
loading, and uses the existing interpreter-compatible scalar semantics for
arbitrary nested addressing.

### Validated implementation batch — 2026-09-07

- Issue #406 adds typed SIMD lowering for `floor`, `ceil`, `round`, and
  `trunc`. AVX2 and AVX-512F use packed floor/ceil/trunc instructions, and
  AArch64 NEON uses FRINTM/FRINTP/FRINTA/FRINTZ. The original x86
  ties-away-from-zero `round` fallback was superseded by Issue #417's exact
  packed AVX2/AVX-512F sequence; SSE2 remains scalar because it has no
  supported packed implementation. Bit-level tests cover ties, NaN,
  infinities, and signed zero.
- Issue #417 adds exact x86 packed ties-away-from-zero `round`: AVX2 uses
  absolute-value plus 0.5, floor, and sign restoration, while AVX-512F uses
  the equivalent packed sequence with a k-masked NaN payload-preserving merge.
  Issue #424 extends the same exact width-2 path to SSE4.1 with `roundpd`
  floor/ceil/trunc primitives and bit-preserving NaN handling. SSE2-only and
  unsupported x86 widths continue to use the exact scalar interpreter fallback.

- PR #124 completed ABI-aware translation assertions and volatile Win64 XMM
  scratch selection; PR #126 completed layout-level libm scratch selection and
  native live-value coverage.
- PR #128 reduced the allocator benchmark from the mid-50 µs range to about
  42–43 µs without changing its workload or allocation policy.
- PR #130 adds an end-to-end depth-100 runtime regression. These changes are
  validated through the repository’s full Linux x86-64, emulated ARM64, WASM,
  Workbench, build/lint, and native Windows lanes as their PRs merge.
- PR #144 adds native-runtime differential property coverage, and fixes a real
  spilled `FloatAbs` scratch-register clobber found by the new Windows lane;
  the feature PR and its promotion PR both pass the complete matrix.
- PR #152 adds arbitrary-byte parser panic-safety property coverage, including
  invalid UTF-8 input through the frontend boundary.
- PR #136 adds an exact AVX2+FMA packed path for `fma`, with scalar fallback
  when FMA is unavailable; PR #138 adds mixed scalar AArch64 emission for
  separate AAPCS64 integer and floating parameter banks. Both are covered by
  the same full platform matrix.
- PR #154 adds AArch64 scalar min/max emission with interpreter-compatible NaN
  behavior and fixes integer negation to use XZR; the feature PR and PR #156
  promotion pass the complete matrix.
- PR #162 implements the React/Vite Workbench against the real
  `forge-wasm-api` artifact boundary, including CodeMirror editing, AST/IR/CFG
  visualization, benchmark charting, and explicit native-target availability
  states; its Workbench production build and full feature matrix pass.
- PR #164 promotes the Workbench to `main`. The Workbench container now runs
  `npm ci`, the smoke test, and the production Vite build in CI.
- PR #170 exposes target-aware x86-64 and AArch64 inspection artifacts through
  `compile_target_artifact_json`, including native bytes, target metadata,
  register intervals for x86-64, and fixed-width AArch64 instruction words;
  unsupported process-local libm calls return an explicit error instead of an
  unserializable function pointer. PR #172 promotes this boundary to `main`.
- PR #181 adds a 500-live-value spill-frame execution regression covering
  heavy allocation pressure, reload/store traffic, and bit-exact native output;
  PR #183 promotes it to `main`.
- PR #185 adds randomized valid-IR optimizer/verifier property coverage with
  512 generated programs and NaN-aware interpreter equivalence; PR #187
  promotes it to `main`.
- PR #193 adds typed straight-line f64 vector IR consumed by the packed
  evaluator; PR #195 promotes it to `main`.
- PR #197 adds stable hand-written EVEX.512 packed-double `vaddpd`, `vmulpd`,
  and `vfmadd231pd` forms with mask/zeroing fields and iced-x86 round trips;
  PR #199 promotes it to `main` and resolves the stable-toolchain decision in
  issue #179. AVX-512 runtime dispatch and masked-tail loops remain open.
- PR #201 keeps AArch64 temporaries in caller-saved ABI registers and rejects
  programs beyond the safe volatile-register budget; PR #203 promotes it to
  `main`.
- PR #209 adds aligned AArch64 save/restore frames for allocated callee-saved
  f64 and i64 temporaries, with native execution regressions; PR #211 promotes
  it to `main`. General spill/reload allocation, full ABI frames, and libm
  calls remain open.
- PR #217 adds a correctness-first straight-line AArch64 i64 spill/reload
  fallback using aligned stack slots and preserved scratch registers, with
  native execution coverage; PR #219 promotes it to `main`. CFG/phi spilling,
  f64 spilling, calls/libm, and general live-range allocation remain open.
- PR #225 adds a correctness-first straight-line all-f64 AArch64 spill/reload
  fallback using aligned stack slots and D29..D31 scratch registers, with
  native execution coverage; PR #227 promotes it to `main`. CFG/phi spilling,
  mixed-value spilling, calls/libm, and general live-range allocation remain
  open.
- PR #231 extends the all-f64 AArch64 spill/reload fallback across structured
  CFGs, re-evaluates direct f64 comparisons at branch sites, and stores f64 φ
  values on their incoming edges; byte-level and native ARM64 execution
  coverage pass the full platform matrix. Mixed-value spilling, calls/libm,
  and general live-range allocation remain open.
- PR #237 adds typed stack spilling for high-pressure straight-line mixed
  f64/i64/bool expressions returning f64, including ABI-bank parameters,
  conversions, literal pools, and native ARM64 execution; the complete PR
  matrix passes. PR #243 extends this to supported structured CFGs, direct
  f64/integer branch comparisons, and typed f64/i64/bool phi edge stores with
  native ARM64 execution; its complete matrix passes. Calls/libm, full ABI
  prologues/epilogues, and general live-range allocation remain open.
- PR #249 adds process-local scalar f64 libm calls on AArch64, including
  AAPCS64 D0/D1 argument marshalling, aligned call frames, X30 link-register
  preservation, unary and binary call coverage, and native ARM64 execution;
  its complete matrix passes. Mixed-signature calls, broader external ABIs,
  and general live-range allocation remain open.
- PR #255 extends scalar AArch64 libm calls to mixed f64/i64/bool parameter
  functions by spilling both AAPCS64 parameter banks in the aligned frame
  before `BLR`, while retaining the supported D0/D1 argument marshalling and
  X30 preservation. Byte-level and native ARM64 coverage pass the complete
  matrix. Arbitrary mixed-signature external calls, broader external ABIs,
  and general live-range allocation remain open.
- PR #278 adds real scalar x86 FMA3 emission (`vfmadd231sd`) with an exact
  interpreter fallback when runtime FMA is unavailable; PR #281 adds the
  deterministic 100,000-expression native differential corpus and fixes the
  edge cases it exposed: unordered float-diamond fusion, four-spill scalar
  FMA scratch pressure, constrained-register aliasing, and Forge's explicit
  signed-zero min/max semantics. PR #281's complete Linux x86-64, emulated
  ARM64, WASM, Workbench, macOS, Windows, Valgrind, and GitGuardian matrix
  passed before merge to `codex/integration`.
- PR #288 adds AVX-512F 8-lane runtime dispatch and stable inline-assembly
  k-masked zeroing load/store tails, with scalar fallback when AVX-512F is
  unavailable. Its complete Linux x86-64, emulated ARM64, WASM, Workbench,
  macOS, Windows, Valgrind, and GitGuardian matrix passed before merge to
  `codex/integration`.
- PR #292 adds an executable native-versus-WASM differential harness using
  the same generated module and exact floating-point bit checks; its complete
  Linux x86-64, emulated ARM64, WASM, Workbench, macOS, Windows, Valgrind,
  and GitGuardian matrix passed before merge to `codex/integration`.
- PR #294 executes typed WASM artifacts in the real Node runtime and checks
  exact `i64` results, canonical bool results, lets, conditionals, bitwise
  operations, and mixed bool/`i64` signatures against the interpreter; its
  complete platform matrix passed before merge to `codex/integration`.
- PR #300 adds register-backed typed native x86-64 calls through an
  ABI-aware executable trampoline, covering System V class-specific and
  Win64 positional argument registers plus exact f64/i64/bool returns; its
  complete platform matrix passed before merge to `codex/integration`.
- PR #304 extends the x86-64 typed trampoline across Win64 positions five
  through eight, including aligned shadow space and packed stack argument
  marshalling for mixed f64/i64 signatures; its complete platform matrix
  passed before merge to `codex/integration`.
- PR #312 extends typed x86-64 calls across System V stack-backed integer and
  floating-point parameters and fixes entry parameter lowering to use parallel
  copies, preventing one incoming ABI register from clobbering another. Its
  complete platform matrix passed before merge to `codex/integration`.
- PR #306 adds native AArch64 typed runtime execution for supported mixed
  AAPCS64 f64-result signatures and straight-line all-i64 signatures, with
  separate floating/integer bank loading and link-register preservation; its
  complete platform matrix passed before merge to `codex/integration`.
- PR #318 completes Win64 nonvolatile register handling: the Windows allocator
  can use RBX/RSI/RDI/R12-R15 and XMM6-XMM15, generated frames preserve the
  active set with aligned saves/restores, and local spills are placed below the
  saved-register area. High-pressure native Windows tests exercise RBX and
  XMM6 allocation and execution; the complete platform matrix passed before
  merge to `codex/integration`.
- PR #324 completes AAPCS64 stack-backed typed parameters: mixed f64/i64/bool
  f64-result signatures can overflow either type-specific register bank, with
  the runtime trampoline allocating aligned stack argument slots and the body
  loading those values after its local frame. Direct native AArch64 execution,
  byte-level emission, and the complete platform matrix pass.
- PR #330 extends AAPCS64 stack-backed typed parameters to straight-line
  all-i64 signatures beyond X0..X7. The emitter addresses incoming stack
  arguments relative to its aligned spill frame, and the typed trampoline
  marshals packed i64 values into those slots. Native ARM64, runtime, and
  complete platform-matrix coverage pass.
- PR #336 adds native AAPCS64 bool results. Comparison values use the
  canonical CSET 0/1 representation, including stack-backed mixed signatures,
  and the typed runtime trampoline validates and returns canonical bool words.
  Native ARM64, runtime, and complete platform-matrix coverage pass.
- PR #340 adds native AArch64 floor, ceil, round, and trunc lowering through
  FRINTM/FRINTP/FRINTA/FRINTZ, including register and stack-spill paths. Round
  uses FRINTA because Forge's interpreter follows Rust's ties-away-from-zero
  `f64::round` semantics. Encoder, native ARM64, runtime, and complete
  platform-matrix coverage pass.
- PR #345 adds exact process-local `fmod` lowering for native x86-64 and
  AArch64 paths, constant folding, and native/runtime bit checks. WASM retains
  an explicit interpreter fallback for portable evaluation because its scalar
  instruction set has no `f64.rem`.
- PR #354 adds exact packed SIMD `min`/`max` lowering and execution across the
  supported vector backends, with bit-exact NaN payload and signed-zero tests
  for every array length from 1 through 100. AVX-512F uses the exact scalar
  lane helper for these two operations because its native min/max instructions
  do not implement Forge's NaN and zero-tie contract; other packed backends
  use vector compare/select and bitwise sign handling. Packed loop codegen,
  richer vector control flow, and general array-mode IR remain open.
- The SIMD array-loop boundary is now implemented in `forge-simd`: supported
  straight-line functions lower to a reusable typed vector body plus an
  explicit `VectorLoop` with induction start/step, full-chunk count, tail
  count, `VecLoad` inputs, and an output `VectorStore`. The packed backends
  execute that body once per full chunk, retain scalar epilogues for SSE2,
  AVX2, and NEON, and retain the AVX-512F masked-tail path. Tail and IEEE
  edge coverage remains element-for-element tested across lengths 1 through
  100. Richer vector control flow and general array-mode IR remain outside this
  narrow boundary.
- Pure structured vector control flow is now implemented in `forge-simd` for
  acyclic `if`/`then`/`else` trees: vector comparisons produce canonical lane
  masks, branch paths are combined with mask algebra, and φ values lower to
  bit-preserving predicated selects. Nested branches, NaN conditions, packed
  tails, and scalar equivalence are regression-tested. Loops, calls/libm, and
  general array-mode memory IR remain outside this boundary.
- Explicit SIMD feature masking is now implemented in `forge-simd`: requested
  capabilities are intersected with the host snapshot before width selection,
  FMA is independently respected by packed dispatch, and `CpuFeatures::scalar`
  forces an exact interpreter fallback. Array evaluation and source-ordered
  reductions expose the mask-aware entry points and regression-test NaNs,
  infinities, signed zero, conditionals, and disabled FMA execution. General
  array-mode memory/loop IR remains outside this boundary.
- Issue #434 adds the first complete source-level array path: `@vectorize
  result[i] = a[i] * b[i] + c[i]` is lexed, parsed, type-checked, lowered to
  an independently verified `forge_ir::array::ArrayFunction` with explicit
  induction/header/body/exit blocks and `Load`/`Store` operations, and
  evaluated through the existing packed widths, scalar epilogues, masked
  tails, and exact interpreter fallback. End-to-end tests compare the source
  form with the scalar expression for every length 1 through 100 and cover
  structured `if` bodies and malformed addressing.
- The AArch64 frame-record boundary is implemented on branch
  `codex/aarch64-frame-record-20260907`: checked GPR and SIMD pair encoders,
  standard x29/x30 frame records for every generated spill/call frame, and
  adjusted incoming-stack addressing. Byte-level tests cover the pair forms;
  the existing native AArch64 spill, CFG, typed-parameter, and libm suites
  exercise the updated prologues and epilogues. General live-range allocation,
  arbitrary mixed-signature external calls, and the remaining vector/WASM
  boundaries stay explicit follow-up scope.
- The AArch64 scalar live-range reuse boundary is implemented on branch
  `codex/aarch64-live-range-20260907` for issue #366: eligible no-call programs
  now use inclusive liveness intervals to reuse dead D/X temporaries. The
  follow-up CFG allocator models block-entry phis, per-edge live values, and
  parallel phi moves, including cycle-breaking scratch registers; genuine
  pressure, calls, stack-backed parameters, and unsupported shapes retain
  their established spill/ABI fallbacks. The focused AArch64 suite covers
  nested structured CFG execution, downstream-phi edge-live preservation,
  register reuse, callee-saved frame use, and simultaneous-live spill cases.
- The AArch64 general scalar-allocation boundary is implemented for issue
  #374 in the current validation batch: CFG interference coloring is now used
  for supported no-call/no-stack-parameter scalar functions, while the existing
  spill and call paths remain explicit fallbacks. The 100,000-expression
  native differential corpus passes after adding regressions for block-entry
  phi ordering, mixed `i64`/`bool` register classes, downstream phi copies,
  and nested branch chains. Arbitrary external-call signatures remain open.
- The WASM f64-remainder boundary is implemented on branch
  `codex/wasm-f64-remainder-20260907` for issue #362: remainder-containing
  modules emit the typed `forge.fmod` import, expose it in artifact metadata,
  and wire it through the Workbench and real Node runtime. Exact cross-backend
  tests cover normal values and signed zero; the host import is deliberately
  absent from modules that do not use `%`.
- The historical phase checkboxes below remain design-history markers. Open
  scope is tracked explicitly in the table above and in each phase’s notes;
  this section must not be read as claiming completion of full AArch64 ABI/code
  generation, fuzz/Miri, native WASM interval/assembly artifacts, or other
  still-open boundaries. The 100,000-expression stress item is
  implemented by PR #281; its historical checkbox remains unchecked only to
  preserve the original phase-history record.

---

## Phase 0 — Bootstrap (14 tasks)

- [ ] 🔴 `cargo new --lib forge`; workspace with 13 member crates per SPEC §17
- [ ] 🔴 `cargo add libc nix region raw-cpuid smallvec bitvec rustc-hash thiserror anyhow`
- [ ] 🔴 `cargo add --dev iced-x86 capstone criterion proptest` — **`iced-x86` is a test oracle only, never used for encoding**
- [ ] 🔴 CI matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu` (QEMU), `wasm32-unknown-unknown`
- [ ] 🔴 `forge-mem` platform detection: Linux / macOS-x64 / macOS-arm64 / Windows, with a clear error if none matches
- [ ] 🔴 macOS: `Info.plist` + entitlements file with `com.apple.security.cs.allow-jit`; codesign step in the Makefile
- [ ] 🔴 `Makefile`: `test`, `test-differential`, `test-encoding`, `bench`, `qemu-aarch64`, `wasm`, `workbench`
- [ ] 🔴 `#![deny(clippy::undocumented_unsafe_blocks)]` workspace-wide — every `unsafe` needs a `// SAFETY:` comment
- [ ] 🔴 `Span { start: u32, end: u32 }` and a diagnostic type with primary/secondary labels
- [ ] 🔴 `cd workbench && bun create vite . --template react-ts`
- [ ] 🔴 `bun add @codemirror/state @codemirror/view @codemirror/language @codemirror/lint d3 @dagrejs/dagre recharts zustand clsx lucide-react`
- [ ] 🔴 `bun add -d tailwindcss postcss autoprefixer @types/d3 vite-plugin-wasm vite-plugin-top-level-await`
- [ ] 🔴 `bunx shadcn@latest init`; add `button card tabs select badge tooltip separator scroll-area resizable slider switch table`
- [ ] 🔴 Hello-world JIT spike: `mmap` → emit `48 89 F8 C3` (`mov rax,rdi; ret`) → `mprotect` → `transmute` → call. **Do this on day one** — if executable memory doesn't work on your platform, nothing else matters.

---

## Phase 1 — Frontend: Lexer, Parser, Types (20 tasks)

- [ ] 🔴 `TokenKind`: number literals, identifiers, `+ - * / % ( ) , < <= > >= == != && || ! & | ^ << >> ~`, `if then else`, `let in`, `@vectorize` — `!` is logical not, `~` is bitwise not, kept as distinct tokens (SPEC §3 "Operators & precedence")
- [ ] 🔴 Hand-written lexer producing `(Vec<Token>, Vec<Diagnostic>)`, never `Result`
- [ ] 🔴 Float literals incl. exponent form; integer literals with `_` separators
- [ ] 🔴 Pratt parser: precedence table per SPEC §3 (`||` → `&&` → `|` → `^` → `&` → `==`/`!=` → relational → `<<`/`>>` → `+`/`-` → `*`/`/`/`%` → unary), left-assoc for arithmetic, right-assoc for none (no `**`)
- [ ] 🔴 Bitwise/shift operators (`& | ^ << >>`) type-checked as i64-only — using them on f64 is a type error with a span
- [ ] 🔴 Prefix: literal, identifier, `(`, unary `-`, unary `!`, `if`, `let`
- [ ] 🔴 Infix: all binary ops, `(` call
- [ ] 🔴 `if cond then a else b` as an **expression**, not a statement
- [ ] 🔴 `let x = e1 in e2` — creates CSE opportunities and a scope
- [ ] 🔴 Intrinsic call parsing with arity validation against a table
- [ ] 🔴 AST arena with `Idx<Expr>`, parallel `spans` vec
- [ ] 🔴 Type checker: `f64`, `i64`, `bool`; implicit `i64 → f64` widening where unambiguous
- [ ] 🔴 Type errors with both operand spans labeled
- [ ] 🔴 Comparison operators produce `bool`; `if` requires a `bool` condition and matching branch types
- [ ] 🔴 Parameter binding: free identifiers become parameters in declaration order
- [ ] 🔴 Constant environment: `--x 3.0` binds `x` as a compile-time constant, enabling folding
- [ ] 🔴 Test: precedence, associativity, unary binding
- [ ] 🔴 Test: type error on `1 + true`
- [ ] 🔴 Test: `if` branch type mismatch
- [ ] 🔴 Test: intrinsic arity mismatch
- [ ] 🟡 Property test: parse(print(ast)) == ast

---

## Phase 2 — SSA IR (26 tasks)

- [ ] 🔴 `Value(u32)`, `Block(u32)` — Copy, indices not pointers
- [ ] 🔴 `Inst` enum: all ~40 variants from SPEC §5
- [ ] 🔴 **`ConstF64(u64)` storing the bit pattern**, not `f64` — `f64` is neither `Hash` nor `Eq`, and GVN needs both
- [ ] 🔴 `Terminator::{ Return, Jump, Branch }`
- [ ] 🔴 `Function { insts, types, spans, blocks, entry, params }` with parallel vecs
- [ ] 🔴 `BlockData { insts: Vec<Value>, term, preds }`
- [ ] 🔴 Builder API: `add()`, `mul()`, `sqrt()`, … each returning a fresh `Value`
- [ ] 🔴 AST → IR lowering for straight-line expressions
- [ ] 🔴 `if` lowering: create then/else/merge blocks, emit `Branch`, insert a φ at the merge
- [ ] 🔴 `let` lowering: bind the name, lower the body — no instruction emitted
- [ ] 🔴 Braun et al. SSA construction: `read_variable` / `write_variable` / `read_variable_recursive`
- [ ] 🔴 **Incomplete-φ handling** to break cycles — without it a back-edge causes infinite recursion
- [ ] 🔴 Trivial-φ removal: a φ whose operands are all the same value, or all the same plus itself
- [ ] 🔴 `preds` maintained incrementally as blocks are linked
- [ ] 🔴 Reverse-postorder traversal for pass ordering
- [ ] 🔴 Dominance tree (Cooper-Harvey-Kennedy iterative algorithm)
- [ ] 🔴 `use_count` / `users` index, maintained by all passes
- [ ] 🔴 `replace_all_uses(old, new)` used by every rewriting pass
- [ ] 🔴 **IR verifier**: every value defined once; every use dominated by its def; φ operand count equals pred count; types consistent per opcode
- [ ] 🔴 **Run the verifier after every pass in debug builds** — catches optimizer bugs at the pass that caused them, not three passes later
- [ ] 🔴 Textual IR printer (the workbench's IR panel and `forge ir` both use it)
- [ ] 🔴 Test: `sqrt(x*x+y*y)` produces exactly 6 instructions
- [ ] 🔴 Test: `if` produces 4 blocks with a correct φ
- [ ] 🔴 Test: verifier rejects a hand-constructed use-before-def
- [ ] 🔴 Test: verifier rejects a φ with the wrong operand count
- [ ] 🟡 Test: dominance tree correct for nested `if`

---

## Phase 3 — Interpreter (Tier 0) (10 tasks)

**Build this early. It is the correctness oracle for everything that follows.**

- [ ] 🔴 `RtValue` enum (`F64(f64) | I64(i64) | Bool(bool)`) defined in `forge-ir`, matching `Function.params`' real per-parameter types (SPEC §3 "Runtime value representation") — not a bare `f64`, since params can be i64 or bool
- [ ] 🔴 `interpret(f: &Function, args: &[RtValue]) -> RtValue` walking the IR
- [ ] 🔴 Every `Inst` variant handled — a missing arm must be a compile error, not a panic
- [ ] 🔴 Block traversal following terminators, with φ resolution based on the incoming block
- [ ] 🔴 Intrinsics via Rust's `f64` methods, matching libm semantics exactly
- [ ] 🔴 `Call` to libm functions
- [ ] 🔴 IEEE-754 correctness: NaN propagation, ±0, ±Inf, subnormals — **no shortcuts**, this is the oracle
- [ ] 🔴 Integer overflow follows wrapping semantics consistently
- [ ] 🔴 Test: known values for every intrinsic
- [ ] 🔴 Test: NaN and Inf propagation through every arithmetic op
- [ ] 🔴 Test: `if` with a NaN comparison (all comparisons with NaN are false)

---

## Phase 4 — Optimizer (32 tasks)

- [ ] 🔴 Pass trait: `fn run(&mut self, f: &mut Function) -> bool` (returns "changed")
- [ ] 🔴 Driver running to a fixed point, capped at 10 iterations, with per-pass stats

**Constant folding**
- [ ] 🔴 Fold all arithmetic, comparison, and intrinsic ops on constants
- [ ] 🔴 **Do not fold in ways that change FP semantics.** Folding `0.0/0.0 → NaN` is correct; folding `x*0.0 → 0.0` is not (x may be NaN/Inf)
- [ ] 🔴 Integer folding uses wrapping arithmetic consistently with the interpreter

**Algebraic simplification**
- [ ] 🔴 Rule table with a `Validity` field: `Always` / `IntOnly` / `FastMathOnly`
- [ ] 🔴 `x+0`, `x-0`, `x*1`, `x/1`, `-(-x)` → always valid
- [ ] 🔴 `x*0→0`, `x-x→0`, `x/x→1`, `x&x→x`, `x^x→0` → **integer only** (NaN breaks all of them for floats)
- [ ] 🔴 `sqrt(x*x)→abs(x)`, `a*b+c→fma` → **fast-math only**
- [ ] 🔴 Commutative canonicalization: lower `Value` index first, so `a+b` and `b+a` unify

**Strength reduction**
- [ ] 🔴 `x * 2^k → x << k`; `x / 2^k → x >> k` with the signed rounding fixup
- [ ] 🔴 `x % 2^k → x & (2^k-1)`
- [ ] 🔴 **Magic-number division** (Granlund-Montgomery) for `x / C` — the most dramatic win, since `idiv` is 20–40 cycles and unpipelined
- [ ] 🔴 `x*3`, `x*5`, `x*9` → `lea` forms
- [ ] 🔴 `pow(x,2) → x*x`; `pow(x,0.5) → sqrt(x)`; `pow(x,-1) → 1/x`

**GVN / CSE**
- [ ] 🔴 Hash-cons on `(opcode, canonical operands)` in reverse-postorder
- [ ] 🔴 Commutative ops canonicalized before hashing — forgetting this halves the hit rate
- [ ] 🔴 Only CSE within a dominating region (a value must dominate its replacement's uses)

**Others**
- [ ] 🔴 Copy propagation
- [ ] 🔴 Dead code elimination: mark from the terminator backward, sweep unmarked
- [ ] 🟡 Reassociation: rebalance associative chains to minimize dependency depth (`((a+b)+c)+d` → `(a+b)+(c+d)`)
- [ ] 🟡 FMA contraction (fast-math only), emitting `Inst::Fma`
- [ ] 🟡 LICM for array mode: hoist loop-invariant computation out of the vectorized loop
- [ ] 🟡 Per-pass statistics: instructions removed, rules fired, dependency depth before/after

**Tests**
- [ ] 🔴 **Test: `-O0` and `-O2` produce bit-identical results** across the whole expression corpus (no fast-math)
- [ ] 🔴 Test: `x*0` is NOT folded for f64; IS folded for i64
- [ ] 🔴 Test: `(a+b)*(a+b)` CSEs to one add
- [ ] 🔴 Test: `a+b` and `b+a` CSE together
- [ ] 🔴 Test: magic division matches `idiv` for all of a large random sample, including `i64::MIN`
- [ ] 🔴 Test: DCE removes an unused subexpression entirely
- [ ] 🟡 Test: reassociation reduces dependency depth on a 8-term sum

---

## Phase 5 — Executable Memory (18 tasks)

**Get this right before writing a single encoder function.**

- [ ] 🔴 `ExecutableBuffer { ptr, len, state: ProtState }`
- [ ] 🔴 Linux/macOS-x64: `mmap(PROT_READ|PROT_WRITE)` → write → `mprotect(PROT_READ|PROT_EXEC)`
- [ ] 🔴 **Never map RWX.** W^X is not optional — an RWX page is exactly the primitive an attacker wants
- [ ] 🔴 Page-size rounding via `sysconf(_SC_PAGESIZE)`
- [ ] 🔴 macOS AArch64: `MAP_JIT` flag + `com.apple.security.cs.allow-jit` entitlement
- [ ] 🔴 macOS AArch64: **`pthread_jit_write_protect_np(0)` before writing, `(1)` after** — do NOT use `mprotect` on `MAP_JIT` pages, it fails
- [ ] 🔴 macOS AArch64: **`sys_icache_invalidate()` after every write.** The i-cache is not coherent with the d-cache on Apple Silicon; skipping this produces intermittent, unreproducible wrong behavior
- [ ] 🔴 Linux AArch64: `__builtin___clear_cache` equivalent (`core::arch::asm!("dc cvau"/"ic ivau"/"dsb ish"/"isb")`)
- [ ] 🟡 Windows: `VirtualAlloc(MEM_COMMIT, PAGE_READWRITE)` → `VirtualProtect(PAGE_EXECUTE_READ)` → `FlushInstructionCache`
- [ ] 🔴 `Drop` impl calling `munmap` / `VirtualFree`
- [ ] 🔴 `write<F: FnOnce(&mut [u8])>(f)` API making the protection dance impossible to skip
- [ ] 🔴 Type-state or runtime assert that `state == Executable` before any call
- [ ] 🔴 `CompiledExpr::call1/call2/callN` with an **arity assert** and a single documented `transmute`
- [ ] 🔴 A code cache: reuse buffers across compilations, with a free-list
- [ ] 🔴 Test: allocate, write `mov rax, 42; ret`, execute, get 42
- [ ] 🔴 Test: buffer is not writable after `make_executable` (expect SIGSEGV in a forked child) — **correction (Phase 5):** on this project's macOS AArch64 dev machine, a `MAP_JIT` write-protect violation empirically raises `SIGBUS`, not `SIGSEGV`; `crates/forge-mem/tests/wx_enforcement.rs` accepts either signal, since both are valid hardware protection-fault signals and which one a given OS/kernel raises isn't part of the portable contract being tested (only that W^X is enforced at all)
- [ ] 🔴 Test: no leaks — allocate/free 10,000 buffers, RSS stays flat
- [ ] 🔴 Test under Miri where possible; valgrind for the rest — **resolved as a documented split:** Miri cannot model raw `mmap`/`mprotect`/`sysconf` syscalls or a transmute-to-function-pointer call, so it cannot meaningfully run this crate's tests on the macOS development machine (see `forge-mem`'s crate-level doc comment); Linux CI now runs `crates/forge-mem/tests/no_leaks.rs` under Valgrind Memcheck with definite/indirect leaks failing the job. Leak-freedom is also checked by the test's empirical high-water-mark RSS comparison.

---

## Phase 6 — x86-64 Encoder (40 tasks)

**Every task here gets a disassembler round-trip test in the same commit.**

**Infrastructure**
- [ ] 🔴 `Assembler { code: Vec<u8>, labels, fixups }`
- [ ] 🔴 `PhysReg` enum for GPRs (RAX..R15) and XMM (XMM0..XMM31) with encoding numbers
- [ ] 🔴 Label/fixup machinery for forward jumps; `bind(label)` patches all pending fixups
- [ ] 🔴 Rel8 vs rel32 jump selection with automatic promotion when the offset doesn't fit — **correction (Phase 6a):** implemented as rel8-if-it-fits-else-rel32 for *backward* jumps (whose distance is known immediately at emit time) and unconditionally-rel32 for *forward* jumps (whose distance isn't known until the target label binds); there is no in-place "promote rel8 to rel32" byte-shifting/reflow step anywhere, since that would require adjusting every later label position and pending fixup for a JIT compiling small expressions — see `docs/superpowers/specs/2026-08-05-phase-6a-x64-encoder-infra-design.md`'s "Jump policy" section for the full justification

**Prefixes — the trap zone**
- [ ] 🔴 `rex(w, reg, index, rm)` emitting only when needed
- [ ] 🔴 **REX.W for 64-bit ops** — without it the op is 32-bit and *zeroes the upper 32 bits*
- [ ] 🔴 **REX.R/X/B for r8-r15** — without it you silently address rax-rdi instead
- [ ] 🔴 **Any REX makes spl/bpl/sil/dil replace ah/ch/dh/bh** — a silent different-register bug
- [ ] 🔴 Operand-size prefix `0x66`, address-size `0x67` where needed

**ModRM / SIB — three mandatory special cases**
- [ ] 🔴 `modrm_reg(reg, rm)` for register-direct (mod=11)
- [ ] 🔴 `modrm_mem(reg, base, disp)` for memory
- [ ] 🔴 **`base == RSP (4)` requires a SIB byte** — ModRM.rm=100 means "SIB follows", so `[rsp]` cannot be encoded directly
- [ ] 🔴 **`base == RBP (5)` with disp=0 must use mod=01 disp8=0** — mod=00 rm=101 means RIP-relative, not `[rbp]`
- [ ] 🔴 **R12 and R13 hit the same two cases via REX.B** — very easy to handle rsp/rbp and forget their twins
- [ ] 🔴 SIB with scale/index/base for `[base + index*scale + disp]`
- [ ] 🔴 RIP-relative addressing for constant pool loads — **note (Phase 6f):** the addressing-mode primitive is built (`lea_reg_riprel`/`movsd_reg_riprel`, fixed `mod=00/rm=101` ModRM pattern with REX.B deliberately never set for it), reusing `Label`/`Fixup`/`bind()` from Phase 6a completely unmodified — the disp32 fixup is patched exactly like a forward `jmp`'s. A full constant-pool *system* (layout, dedup, placement after the code) is explicitly out of scope for this slice and remains Phase 7's job; this bullet is only the primitive that system will need. Details: `docs/superpowers/specs/2026-08-09-phase-6f-x64-calling-convention-riprel-design.md`

**Scalar integer instructions**
- [ ] 🔴 `mov` r/r, r/imm32, r/imm64 (`movabs`), r/m, m/r
- [ ] 🔴 `add` `sub` `imul` `and` `or` `xor` — r/r and r/imm forms
- [ ] 🔴 `neg` `not` `inc` `dec`
- [ ] 🔴 `shl` `shr` `sar` — imm8 and CL forms
- [ ] 🔴 `lea` — including the 3-operand `lea r, [a + b*k]` used by strength reduction
- [ ] 🔴 `cmp` `test`; `setcc`; `cmovcc` — **note (Phase 6c):** all four built (`cmp` as a new `AluOp` variant, `test` via its own `test_reg_reg`/`test_reg_imm`), plus a shared `ConditionCode` enum covering all 16 x86-64 condition codes (not just the 6 forge's current i64 comparisons need) reused by `setcc`/`cmovcc` and by `jcc` below. `jcc` itself shipped in this same slice, not bundled with `push`/`pop`/`call`/`ret` per this bullet's original wording — see the next bullet's correction. Details: `docs/superpowers/specs/2026-08-08-phase-6c-x64-comparisons-design.md`
- [ ] 🔴 `imul` 128-bit form for magic division; `idiv` — **note (Phase 6d):** both built (`imul128_reg`/`idiv_reg`), plus `cqo` alongside them even though it isn't literally in this bullet's wording — `idiv`'s RDX:RAX dividend pair is close to unusable without a way to sign-extend RAX into it first. Also delivered in this slice: `neg`/`not`/`inc`/`dec` and `shl`/`shr`/`sar` (previous two bullets) and `lea` including the 3-operand scaled-index form (bullet above), all via the same golden-byte + `iced-x86` round-trip discipline as 6a-6c. Details: `docs/superpowers/specs/2026-08-08-phase-6d-x64-shifts-lea-idiv-design.md`
- [ ] 🔴 `push` `pop` `call` `ret` `jmp` `jcc` — **correction (Phase 6c):** `jmp` (Phase 6a) and `jcc` (Phase 6c) are both implemented; `push`/`pop`/`call`/`ret` are not. This bullet's original grouping doesn't reflect how the work was actually split: `jcc` was deliberately pulled out and built alongside `cmp`/`test`/`setcc`/`cmovcc` instead, since a conditional branch plus a comparison is the coherent unit forge's `if`/`else` needs to compile at all, whereas `push`/`pop`/`call`/`ret` are real calling-convention work closer in spirit to Phase 7 ("Instruction Selection & Prologue"). See `docs/superpowers/specs/2026-08-08-phase-6c-x64-comparisons-design.md`'s scope note. — **note (Phase 6f):** the remaining four are now built too: `push_reg`/`pop_reg` (opcode-plus-register, no ModRM, the same shape `mov_reg_imm`'s `movabs` form uses), `call_reg` (indirect, `FF /2`) and `call_rel32` (direct, `E8 rel32`, reusing `Label`/`Fixup`/`bind()` from Phase 6a exactly like `jmp` — no rel8 short form, since `call` doesn't have one), and `ret` (bare `C3`). Only the register/memory/immediate `push`/`pop` forms and `ret`'s imm16 stack-cleanup form remain deliberately unbuilt (no consumer — forge's callee-saved save/restore only ever pushes/pops a register, and the imm16 form is stdcall-style callee cleanup, not used by SysV or Win64). This closes out every 🔴-blocking item in this section. Details: `docs/superpowers/specs/2026-08-09-phase-6f-x64-calling-convention-riprel-design.md`

**SSE2 scalar float**
- [ ] 🔴 `movsd` `movapd` `movq` (xmm↔gpr)
- [ ] 🔴 `addsd` `subsd` `mulsd` `divsd` `sqrtsd`
- [ ] 🔴 `minsd` `maxsd` — note these are **not** commutative w.r.t. NaN, matching the interpreter matters
- [ ] 🔴 `andpd`/`xorpd` for `abs` and `neg` via sign-mask constants
- [ ] 🔴 `ucomisd` + `setcc` for comparisons
- [ ] 🔴 `cvtsi2sd` `cvttsd2si`
- [ ] 🔴 `roundsd` (SSE4.1) for floor/ceil/round/trunc — **note (Phase 6e):** all of this section built: `movsd_reg_reg`/`movsd_reg_mem`/`movsd_mem_reg`, `movapd_reg_reg`, `movq_gpr_to_xmm`/`movq_xmm_to_gpr` (bullet above); the `SseOp`-driven `addsd`/`subsd`/`mulsd`/`divsd`/`sqrtsd`/`minsd`/`maxsd` family via `sse_reg_reg`; `andpd_reg_reg`/`xorpd_reg_reg` as raw 2-operand bitwise primitives ONLY — composing them into actual `abs`/`neg` (materializing a sign-mask constant via `mov_reg_imm` + `movq_gpr_to_xmm`, or eventually a RIP-relative constant pool) is deliberately deferred to Phase 7's instruction-selection layer, not built in this slice; `ucomisd_reg_reg` (reusing 6c's `ConditionCode`/`setcc`/`jcc`/`cmovcc` machinery entirely unmodified, documented for use with the unsigned condition codes float comparisons require); and `cvtsi2sd`/`cvttsd2si`. `roundsd` itself is genuinely SSE4.1 not SSE2, as this bullet's own parenthetical already says; the encoder was still built here since CHECKLIST groups it with the SSE2 bullets — runtime CPUID feature detection gating its availability is a separate, later concern (Phase 10's `CpuFeatures`), not addressed by this slice. Details: `docs/superpowers/specs/2026-08-09-phase-6e-x64-sse2-scalar-float-design.md`

**VEX / AVX**
- [ ] 🟡 2-byte and 3-byte VEX emitters
- [ ] 🟡 **`vvvv` field is INVERTED (`!reg & 0xF`)** — the classic VEX bug
- [ ] 🟡 `vaddsd` `vmulsd` `vsqrtsd` etc. — 3-operand, non-destructive
- [ ] 🟡 `vfmadd213sd` / `vfmadd231sd` (FMA3)
- [ ] 🟡 Packed forms `vaddpd` `vmulpd` for 128/256-bit
- [ ] 🔵 EVEX prefix + AVX-512 packed ops with k-mask registers

**Verification**
- [ ] 🔴 `disassemble(&[u8]) -> Vec<String>` via `iced-x86`
- [ ] 🔴 **Round-trip test for every single instruction emitter** — assemble, disassemble, compare to intended text
- [ ] 🔴 Golden-file tests: expression → expected exact hex bytes
- [ ] 🔴 Test the rsp/rbp/r12/r13 ModRM cases explicitly, all four

---

## Phase 7 — Instruction Selection & Prologue (22 tasks)

- [ ] 🔴 `MachineInst` enum sitting between IR and encoding — **note (Phase 7a):** built exactly as scoped, in `crates/forge-x64/src/machine_inst/` (split into `mod.rs`/`tests.rs` in Phase 7b) — a flat enum, one variant per real x86 operation family, always in 3-address SSA form even for the x86 ops that are 2-address-destructive on real hardware (`dst` distinct from its operands; the copy this implies is not inserted here, see the next bullet's note). Virtual registers are `forge_ir::Value` reused directly, no separate `VReg` type — this slice also resolved SPEC.md's pipeline-ordering ambiguity (§4's diagram had register allocation running *before* instruction selection; corrected to match what actually got built: instruction selection now produces `MachineInst` over virtual registers first, and Phase 8 assigns physical registers to them after). `select(&Function) -> SelectedFunction` walks blocks in reverse postorder and lowers every `forge_ir::Inst` variant via an exhaustive match with no wildcard arm — `Phi` emits nothing (deferred to Phase 8's SSA deconstruction, safe only because today's CFG has no critical edges, unenforced), `Call` and `Rem` on `f64` operands both panic with clear deferral messages (`Call` ships in 7e; float remainder/fmod has no native x86 instruction and no libm route yet — deferred outright rather than approximated, unlike `Fma`). Synthetic values minted for `Fma`/`Abs`/`Neg` decomposition (seeded one past the IR's highest real `Value`, collision-free since `Value` numbering is append-only) are tracked in `SelectedFunction::synthetic_types`. Details: `docs/superpowers/specs/2026-08-09-phase-7a-machine-inst-selection-design.md`
- [ ] 🔴 Tree-tiling selection: maximal munch over the IR DAG — **note (Phase 7a):** only the baseline case shipped in this slice — a 1-to-one (or, for `Fma`/`Abs`/`Neg`, 1-to-few) lowering of each IR node in isolation, matched on the node's own opcode plus its operands' `Ty` where the same `Inst` variant is shared across `i64`/`f64` (`Add`/`Sub`/`Mul`/`Div`/`Rem`/`Neg`/`Cmp`). This is deliberately *not* the genuine multi-node tree-tiling/maximal-munch pattern matching this bullet describes — addressing-mode folding (fusing an `Add(Mul(b, k), c)` tree into one `lea`'s effective address), `lea` synthesis, and `Select`→`cmov`/blend diamond-pattern recognition all require looking across multiple IR nodes at once, and are explicitly deferred to a future Phase 7b slice; this bullet stays open until then. See the design doc's "Out of scope (deferred)" list for the full accounting.
- [ ] 🔴 **Two-address fixup**: x86 `add` is `dst += src`, but SSA is 3-address. Insert `mov dst, a` before `add dst, b`, unless a coalescing hint already put `a` in `dst` — **note (Phase 7b):** built as coalescing-*hint* generation only, not copy insertion — `SelectedFunction::coalescing_hints`, populated by `compute_coalescing_hints` over the finished `Vec<MachineInst>`, records a `dst -> lhs`/`dst -> src` hint for every 2-address-destructive op (excluding `IntDiv`/`IntRem`, whose fixed-`RAX`/`RDX` constraint is a different, not-yet-built fixed-register hint, and excluding `Lea`, which is genuinely non-destructive). Actually inserting the `mov` (or skipping it when the hint is honored) is a post-Phase-8 emission-time decision, per 7a's design doc, not this slice's job. Details: `docs/superpowers/specs/2026-08-09-phase-7b-two-address-hints-lea-synthesis-design.md`
- [ ] 🔴 Addressing-mode folding: `Load{base, offset}` folds into the memory operand of the consuming instruction — **note (Phase 7b):** NOT built, and deliberately so — `forge_ir::Inst` has no `Load`/`Store` variant at all, and forge's language has no arrays/pointers/memory operations of any kind, so there is nothing for this bullet to fold today. Stays open, to be revisited if/when the language grows memory operations. Details: `docs/superpowers/specs/2026-08-09-phase-7b-two-address-hints-lea-synthesis-design.md`
- [ ] 🔴 `lea` synthesis for `a + b*k + c` — **note (Phase 7b):** built — `MachineInst::Lea`, recognizing an `Add(scaled-index, c)`/`Add(c, scaled-index)` shape where scaled-index is EITHER `Mul(b, k)` for `k ∈ {2,4,8}` (Tier 1/no-optimizer input, where a multiply-by-constant literally looks like `Mul`) OR its strength-reduced `Shl(b, s)` form for `s ∈ {1,2,3}` (Tier 2/optimized input — `crates/forge-opt/src/strength.rs`'s `StrengthReduceShifts` pass unconditionally rewrites the former into the latter before selection runs, so `Shl` is the shape that actually occurs on realistic optimized input). A whole-function pre-pass (`find_fully_fusable_scaled_indices`) suppresses the fused `Mul`/`Shl`'s own now-dead standalone computation exactly when every one of its uses was absorbed by fusion — DAG-aware, not just tree-shaped. Only the two-term shape is recognized: `forge_ir::Inst::Add` is strictly binary, so a genuine three-additive-term chain spanning two real `Add`s is out of scope, an explicit scope reduction not an oversight. Details: `docs/superpowers/specs/2026-08-09-phase-7b-two-address-hints-lea-synthesis-design.md`
- [ ] 🔴 `Select` → `cmov` (integer) or `vblendvpd` / `minsd`+`maxsd` idioms (float) — branchless where profitable — **note (Phase 7b):** explicitly deferred to **Phase 7f**, a new, concretely-named slice (not an open-ended "future work") — see the design doc's "Why Select→cmov is deferred" section: unlike this slice's other bullets, it's a genuine optimization (not a correctness requirement, since `if`/`else` already lowers correctly via `Branch`+`Phi`) and needs a fundamentally different mechanism — recognizing a multi-block diamond CFG shape before the per-`Value` selector walk, not an incremental match-arm — best built once Phase 8 exists and its performance value can actually be measured. Details: `docs/superpowers/specs/2026-08-09-phase-7b-two-address-hints-lea-synthesis-design.md` — **note (Phase 7f):** built — but narrower than this bullet's own wording implies, an explicit, stated scope reduction, not an oversight. Only the **empty-arm diamond** shape fuses: `if cond then t else e` where the `then`/`else` blocks compute nothing of their own (each is just a `Jump` to the merge block feeding one of the two `Phi` operands directly) — a genuine multi-block CFG pattern recognized by `find_fusable_diamonds` (`crates/forge-x64/src/machine_inst/mod.rs`) before per-`Value` selection runs, exactly as 7b's deferral note anticipated. General **arm-computation** fusion (e.g. folding `a*c`/`b*c` from `if a>b then a*c else b*c` into the cmov itself) is explicitly NOT built — out of scope for this slice. Two fusion shapes: (1) `MachineInst::IntCmov` for the general integer/bool case, hard-gated to `Ty::I64`/`Ty::Bool` only (no float cmov path — SSE has no direct conditional-move instruction over XMM registers); (2) `DiamondFusion::FloatMinMax` recognizing the min/max idiom via `minsd`/`maxsd`, but only a corrected 4-row table of **strict** `<`/`>` comparisons — `CmpOp::Lt`/`CmpOp::Gt` with `then`/`else` values matching the compared operands in one of two orientations each. `Le`/`Ge` comparisons and diamonds whose `then`/`else` values are a third value distinct from both compared operands deliberately do NOT fuse into `minsd`/`maxsd` (IEEE-754 NaN/-0.0 propagation differs between the branching form and the idiom for those shapes — fusing them would be a silent semantics change, not just a missed optimization). "Branchless where profitable" is satisfied here by **design-time reasoning only** — the empty-arm shape's branch-misprediction-elimination case — not by measurement: no real profitability benchmark exists yet, since it's still blocked on task #68's not-yet-built emission pipeline (`MachineInst` → real `Assembler` bytes), matching this project's established honesty convention of not claiming a performance win it hasn't measured. Corpus-wide regression coverage: `crates/forge-x64/tests/diamond_fusion_corpus.rs`. Separately, and unusually significant: this phase's own implementation and review process found and fixed a real correctness bug in already-shipped Phase 8a code — `crates/forge-regalloc/src/liveness.rs` was silently dropping a fused diamond's CFG successor edge, a latent bug that predated 7f but was only activated (and caught) once 7f's fusion started producing the CFG shapes that exposed it; the potential failure mode was silent register-reuse corruption. Fixed and verified in commit `0c0477e`, on top of this phase's own `cea019a`. Details: `docs/superpowers/specs/2026-08-10-phase-7f-select-cmov-diamond-fusion-design.md`
- [ ] 🔴 Constant pool: f64 constants placed after the code, loaded RIP-relative — **note (Phase 7c):** the pool *data structure* is built — `ConstantPool`/`PoolIndex` in `crates/forge-x64/src/machine_inst/mod.rs`, deduplicating interning of raw `u64` bit patterns (`intern` is the only way to obtain a `PoolIndex`), re-exported from `lib.rs`, and threaded through as `SelectedFunction::pool`. `MachineInst::LoadImmF64` now carries `pool_index: PoolIndex` instead of embedding `bits: u64` directly, so two occurrences of the same f64 literal in one function intern to the same slot. What this slice does NOT do — and what this bullet's own wording ("placed after the code, loaded RIP-relative") is actually about — is byte emission: laying the pool's bytes out after the code and emitting real `movsd_reg_riprel`/`lea_reg_riprel` (Phase 6f) with patched offsets. That's fundamentally a byte-emission-time concern needing a real `Assembler` and real `PhysReg` assignments, neither of which exist yet; it's deferred to the same post-Phase-8 "final wiring" step already established by 7a's design doc, which this slice's pool is built to feed. Stays open until that wiring lands. Details: `docs/superpowers/specs/2026-08-09-phase-7c-constant-pool-design.md` — **note (Phase 9a):** the base `MachineInst` → real `Assembler` bytes translation now exists, in a new crate `crates/forge-emit` (`translate_inst` in `translate.rs`, `emit_body` in `layout.rs`, constant-pool placement in `const_pool.rs`). This bullet's own wording is now directly satisfied: `alloc_pool_labels`/`place_pool` lay the pool's bytes out after the code (label allocation happens before translation, byte placement after, deliberately kept separate), and `LoadImmF64`/the `FloatAbs`/`FloatNeg` sign-mask constants emit real RIP-relative loads against patched label offsets. Scope of this slice more broadly: register-only operands (`Location::Reg`, never `Location::Spill`); `Param` and `CallLibm` are not yet implemented (real panics naming the deferring sub-slice, not silent gaps); `IntDiv`/`IntRem`/`Shl`/`Shr`/`Sar` handle the common case but not third-party register-clobber/CL-occupied displacement — see bullet 253/275/279 above, still unbuilt. Real control flow (`Jump`/`Branch`, forward and backward) and `IntCmp`/`FloatCmp`/`IntCmov` (including an alias-safe zero-extension ordering fix found while implementing) also work end-to-end, with the return value placed in the correct ABI register (`xmm0`/`rax`) per the value's real type. Verified primarily via iced-x86 disassembly, which is architecture-independent and is the real, always-running verification bar for this slice; a handful of execution assertions through `forge-mem` exist as a bonus check only, `#[cfg(target_arch = "x86_64")]`-gated, and are NOT currently exercised by this project's CI (which runs on macOS ARM) or by this dev machine — they have never actually been run, so treat them as unverified extra coverage, not as load-bearing proof. Remaining gaps (`Param`/`CallLibm`/spill reload-store/coalescing-elision/phi-resolution/prologue-epilogue wiring) are Phase 9b-9f. Details: `docs/superpowers/specs/2026-08-11-phase-9a-forge-emit-skeleton-design.md`
- [ ] 🔴 Sign-mask constants for `abs`/`neg` — **note (Phase 7c):** `MachineInst::FloatAbs`/`FloatNeg` now carry `mask_pool: PoolIndex` instead of `mask_tmp: Value` — the fixed masks (`0x7FFF_FFFF_FFFF_FFFF` for abs, `i64::MIN` for neg) are interned once each into the shared `ConstantPool`, so every `Abs`/`Neg` in a function shares one pool entry per mask instead of each call site minting its own synthetic `Value` + `LoadImmI64`; that whole `fresh()`/`LoadImmI64` sequence is gone from these two `select_inst` arms. `intern` dedupes purely on raw bits, so `i64::MIN`'s bits and `-0.0f64`'s IEEE encoding are bit-identical and deliberately collide onto the same `PoolIndex` when both appear in one function — verified independently, not just taken on the design doc's word (`(-0.0f64).to_bits() == i64::MIN as u64 == 0x8000_0000_0000_0000`); this is safe because the load *strategy* is determined by which `MachineInst` variant references the index, never by inspecting the pool entry itself. As with the bullet above, this is the pooling/dedup half only — actually materializing the mask via a GPR round-trip + `andpd`/`xorpd` sourced from a real RIP-relative address is still the deferred post-Phase-8 wiring step's job. Details: `docs/superpowers/specs/2026-08-09-phase-7c-constant-pool-design.md`
- [ ] 🔴 Prologue: `push rbp; mov rbp, rsp; sub rsp, N` with N = spill-slot bytes — **note (Phase 7d):** built — `emit_prologue(asm, callee_saved: &[PhysReg], spill_bytes: u32)` in `crates/forge-x64/src/prologue.rs`, calling real `Assembler` methods (`push_reg`/`mov_reg_reg`/`alu_reg_imm`) to emit `push rbp; mov rbp, rsp; <push each callee_saved reg>; [sub rsp, N]`. `N` isn't the raw `spill_bytes` — it's computed by the shared `padded_spill_bytes` helper (see the "Stack alignment" bullet's note below). Parameterized rather than fed real regalloc output, per this project's established Phase 7/8 circular-dependency resolution (Phase 8's real register-allocation output becomes the real input once it exists). System V only; Win64 deliberately deferred (different callee-saved set, needs `movsd`-to-memory for XMM6-15 instead of push/pop, and this project's CI has no Windows target to verify it against) — see the design doc's "Why System V only" section. Details: `docs/superpowers/specs/2026-08-09-phase-7d-prologue-epilogue-design.md`
- [ ] 🔴 **Stack alignment: rsp must be 16-byte aligned at every `call`.** The return address pushed by `call` makes rsp ≡ 8 mod 16 on entry, so the frame size must account for that. Getting this wrong crashes inside libm with a `movaps` fault — **note (Phase 7d):** built — `padded_spill_bytes(num_callee_saved: usize, requested: u32) -> u32` in `crates/forge-x64/src/prologue.rs`, a shared pure function called identically by both `emit_prologue` and `emit_epilogue` (never threaded as a separately-computed value between the two call sites) so they can never disagree: it pads `requested` up so the total frame (callee-saved pushes + this) is a multiple of 16, correctly accounting for the `rsp ≡ 8 (mod 16)` state left by `call`'s return-address push plus `push rbp`. Tested for the degenerate (0, 0) case, already-16-aligned requests, misaligned requests needing padding, and both odd/even callee-saved counts (odd counts need padding even when `spill_bytes` is 0, purely from the push count itself). Details: same design doc as above.
- [ ] 🔴 Callee-saved register save/restore, only for registers actually used — **note (Phase 7d):** built — `SYSV_CALLEE_SAVED: &[PhysReg]` (`Rbx`, `R12`-`R15`) in `crates/forge-x64/src/prologue.rs`, deliberately excluding `Rbp` — `Rbp`'s save/restore is baked unconditionally into `emit_prologue`/`emit_epilogue`'s own `push rbp`/`pop rbp`, never passed in by the caller. Both functions `assert!` (not `debug_assert!`, matching this project's "caller bugs fail loudly in release too" precedent from 6a's `bind()`) if `Rbp` appears in the caller-supplied `callee_saved` slice — covered by `#[should_panic]` tests for both functions. "Only for registers actually used" (this bullet's own wording) is honored by construction: `emit_prologue`/`emit_epilogue` push/pop exactly the slice they're handed, nothing more — deciding WHICH registers were actually used by a given function is Phase 8's job (regalloc), not this slice's. Details: same design doc as above.
- [ ] 🔴 Epilogue: `mov rsp, rbp; pop rbp; ret` (or `leave; ret`) — **note (Phase 7d):** built — `emit_epilogue(asm, callee_saved: &[PhysReg], spill_bytes: u32)` in `crates/forge-x64/src/prologue.rs`, emitting `[add rsp, N]; <pop each callee_saved reg, REVERSE order>; pop rbp; ret`. Deliberately NOT this bullet's own suggested `mov rsp, rbp`/`leave` shortcut — that shortcut is only correct when there are zero callee-saved registers to restore, since it walks `rsp` back past the saved data without ever popping it into the registers, silently corrupting them once any exist (a correctness bug against SPEC.md §18's "preserve all callee-saved registers" property, not a style choice). This still degrades correctly to the simple `pop rbp; ret` 2-instruction case when `callee_saved` is empty and `spill_bytes` is 0, with no special-cased branch needed. A full round-trip test (`emit_prologue` then `emit_epilogue` with matching inputs) confirms symmetry via `iced-x86` disassembly. Details: same design doc as above.
- [ ] 🔴 Red zone (System V): 128 bytes below rsp usable without adjustment in leaf functions
- [ ] 🔴 **Win64 shadow space: 32 bytes allocated by the caller** before any `call`. Omitting it corrupts the callee's frame
- [ ] 🔴 libm call sequence: spill live caller-saved registers, align, `call`, restore — **note (Phase 7e):** NOT built — this slice built only the selection-level representation, `MachineInst::CallLibm { dst, func, args }` in `crates/forge-x64/src/machine_inst/mod.rs` (records WHAT gets called, with WHICH SSA args, WHERE the result goes), plus real FFI address resolution via `libm_address(func: LibFunc) -> i64` in the new `crates/forge-x64/src/libm.rs`. The actual byte-level sequence this bullet names — spilling live caller-saved XMM registers, aligning `rsp` at the call site, emitting `mov_reg_imm`+`call_reg`, restoring spilled registers — is unbuilt. It needs real liveness/`PhysReg` information that only exists once Phase 8's register allocator runs, so it's deferred to the not-yet-started "Final code-emission pipeline: MachineInst → real Assembler bytes" task. Stays open. Details: `docs/superpowers/specs/2026-08-09-phase-7e-libm-call-design.md`
- [ ] 🔴 **All XMM registers are caller-saved on System V** — any `sin`/`cos` call clobbers every float register, which is why `sin(x)+cos(y)` spills and `sqrt(x)+sqrt(y)` doesn't — **note (Phase 7e):** still just an ABI fact, unenforced by any shipped code — no spilling logic exists yet (see bullet above). `CallLibm`'s selection-level representation doesn't touch physical registers at all, so this fact isn't actionable until the deferred emission-pipeline task builds real spill code around real calls. Stays open.
- [ ] 🔴 Argument marshalling per ABI: SysV (rdi/rsi/…, xmm0-7) vs Win64 (rcx/rdx/r8/r9, xmm0-3) — **note (Phase 7e):** NOT built — `CallLibm`'s `args: SmallVec<[Value; 2]>` field carries the SSA values that will need marshalling (a direct, mechanical copy of `forge_ir::Inst::Call`'s own `{func, args}` shape), but actually moving them into `xmm0`/`xmm1` per the ABI needs real `PhysReg` assignments from Phase 8 to know whether a `movapd` is even necessary. Deferred to the emission-pipeline task; Win64 remains entirely out of scope, same reasoning as Phase 7d. Stays open.
- [ ] 🔴 Return value in `xmm0` (float) or `rax` (int) — **note (Phase 7e):** NOT built — `CallLibm`'s `dst: Value` field records where the IR-level result belongs, but the actual "move `xmm0` into `dst`'s assigned register/slot" step doesn't exist until Phase 8 assigns `dst` a real location. Deferred to the emission-pipeline task. Stays open.
- [ ] 🔴 Test: generated function callable from Rust via `extern "C"` — **note (Phase 7e):** NOT built — requires a fully emitted, runnable machine-code function, which needs the not-yet-built MachineInst → real Assembler bytes emission step (itself blocked on Phase 8). Stays open.
- [ ] 🔴 Test: callee-saved registers unchanged across a call (assert with inline asm) — **note (Phase 7e):** NOT built, same reason as above — no real call sequence exists yet to preserve callee-saved registers around. Stays open.
- [ ] 🔴 Test: stack alignment holds at every call site (checked with a probe function that faults on misalignment) — **note (Phase 7e):** NOT built, same reason as above — no real call-site `rsp` alignment logic exists yet to probe. Stays open.
- [ ] 🔴 Test: an expression calling `sin` and `cos` produces the correct value — **note (Phase 7e):** NOT built as a full integration test (same reason as above — needs real emitted, runnable code). What this slice adds instead, as a down payment: `crates/forge-x64/src/libm.rs`'s own unit tests (`all_six_addresses_are_real_and_pairwise_distinct`, `resolved_addresses_are_bit_exact_against_rust_std_math`, `log_is_natural_log_not_log10`) give real, bit-exact-verified correctness for the *address-resolution* half of `sin`/`cos` only, not the end-to-end compiled-and-executed expression this bullet actually asks for. Stays open.

---

## Phase 8 — Register Allocation (24 tasks)

- [ ] 🔴 Linearize the IR: assign a sequential number to every instruction in RPO — **note (Phase 8a):** satisfied without a numbering pass — `select()` already builds `SelectedFunction::insts` by walking `reverse_postorder(func)`, so the `Vec` index IS the linear instruction number. What this slice actually had to add was block-boundary recovery: `SelectedFunction::block_starts: Vec<(Block, usize)>` (`crates/forge-x64/src/machine_inst/mod.rs`), recording each block's first `insts` index in that same RPO order, populated inside `select()`'s existing walk. It could not be reconstructed externally: `insts` is a flat sequence with no boundary markers, and the IR-`Inst`-to-`MachineInst` count is not 1:1 (`Fma` emits 2, `Phi` and lea-fusion-suppressed `Mul`/`Shl` emit 0), so only `select()`'s own walk can record it correctly. A block's end is the NEXT ENTRY'S start by list position — deliberately not "the next larger value", since a block selecting to zero `MachineInst`s makes two adjacent entries share one start. Additive and backward-compatible; no existing field, test, or golden expectation changed. Details: `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md`
- [ ] 🔴 Liveness analysis: backward dataflow, `live_in`/`live_out` per block — **note (Phase 8a):** built — `compute_liveness(func, selected) -> Liveness` in the new `crates/forge-regalloc/src/liveness.rs`, a standard backward per-block fixpoint (`live_out[B] = ⋃ live_in[S]`; `live_in[B] = uses[B] ∪ (live_out[B] − defs[B])`) iterated until no set changes. Per-block `uses`/`defs` come from exhaustive, wildcard-free `reads_of`/`def_of` matches over every `MachineInst` variant (same discipline as `select_inst` — a new variant fails to compile rather than being silently ignored), and successors come from the real `Jump`/`Branch` terminators in `insts`, never from `BlockData::preds` (which is `Builder`'s own bookkeeping a hand-built `Function` can leave stale or unpopulated). Because it's a fixpoint and not a single ordered pass, it is already loop-correct by construction — see the back-edge bullet below. Details: same design doc.
- [ ] 🔴 Build `Interval { value, start, end, reg_class, hint, fixed, spill_weight }` — **note (Phase 8a):** built — `Interval` with exactly the seven fields this bullet names, plus `RegClass { Gpr, Xmm }` and `RegClass::of(Ty)`, in `crates/forge-regalloc/src/interval.rs`; constructed by `build_intervals(func, selected) -> Vec<Interval>` in `crates/forge-regalloc/src/intervals.rs` from the real liveness dataflow above (not a per-block approximation — a value's `end` covers every position it is live across, including blocks it merely passes through). TWO deliberate, documented deviations from SPEC.md §7's struct sketch: (a) `hint` is `Option<Value>`, not `Option<PhysReg>` — at interval-construction time no value has been assigned a register yet, so `Option<PhysReg>` literally cannot express "co-locate with whatever *that* value gets"; 8b resolves the `Value` through its own scan-time assignment map. (b) `[start, end]` is INCLUSIVE, not the `[start, end)` SPEC.md's doc comment states — `end` is the position of the last read and the value is live AT it, so `[0,2]` and `[2,4]` DO overlap and 8b/8d's overlap predicates must use `a.start <= b.end && b.start <= a.end`. `spill_weight` is always `0.0` here (Phase 8c populates it) and `fixed` is always `None` (see the `fixed`-registers bullet below — that is a real finding, not an omission). The returned `Vec` is sorted by `(start, end, value)`: construction is `HashMap`-backed, and an unsorted return would make register assignment and therefore emitted machine code nondeterministic across runs on identical input. Details: same design doc.
- [ ] 🔴 Intervals must extend across the whole loop body for values live around a back-edge — **note (Phase 8a):** a documented no-op, deliberately not silently skipped. This project's IR has no loop construct today — neither the front-end grammar nor `forge-syntax` can produce a back edge, and `forge_ir::dominance` operates over a DAG-shaped CFG — so there is nothing for this bullet to extend across yet. It needs no future rewrite either: the liveness dataflow above is a fixpoint iteration that handles back edges automatically once the CFG actually has any, rather than a single-pass algorithm that assumes one visitation order converges. Same treatment as Phase 7a's φ critical-edge caveat: an invariant that is true by construction today and left unenforced. Stays open until loops exist and this can be genuinely tested.
- [ ] 🔴 φ handling: an interval spans from the φ to all its incoming definitions — **note (Phase 8a):** built, and it took more than the bullet's own wording implies. `merge_phi_intervals` (`crates/forge-regalloc/src/intervals.rs`) unions every φ destination with all of its incoming values into one shared `[min start, max end]` range via UNION-FIND, not independent per-φ pairwise merges — a φ can feed another φ and two φs can share an incoming value, and merging in `func.insts` order would produce order-dependent ranges. A φ destination is also explicitly SEEDED with an interval at its owning block's first position: Phase 7a's `Phi` emits no `MachineInst` at all, so without the seed a φ dst that is genuinely read (by a `Return`, say) would get no interval and therefore no register — a bug caught in plan review before implementation. A critical-edge `assert!` tripwire (not `debug_assert!`, per this project's "invariant bugs fail loudly in release too" precedent) re-verifies the assumption Phase 7a's φ-lowering depends on, with both counts derived from real terminators; it is tested in BOTH directions — it never fires on any currently-producible if/else program, and it does fire on a hand-built critical edge. `successors_of` dedupes a degenerate `Branch { then_: X, else_: X }` to one successor so the tripwire cannot misfire on a non-critical edge (unreachable today, but Phase 7f's diamond fusion could plausibly synthesize that shape). The range merge alone turned out NOT to be sufficient to make φ-coalescing real — see the hints bullet below.
- [ ] 🔴 Sort intervals by start point — **note (Phase 8b):** built — `LinearScan::run()` (`crates/forge-regalloc/src/linear_scan.rs`) sorts by `(start, end, value)`, not by `start` alone, deliberately: `build_intervals` (8a) already returns that exact key order for determinism, and a bare `start` sort would reintroduce the nondeterminism 8a closed (φ-merged group members all share one identical `start`, so `start` alone leaves their relative order to whatever the sort happens to do with equal keys — and since `pick_register` resolves a hint against whatever is *already assigned*, a different tie-break produces different machine code on identical input). The sort lives inside `run()` rather than being assumed of the caller, so `LinearScan` is correct even when handed an unsorted `Vec<Interval>` (which its own hand-built fixture tests do). Details: `docs/superpowers/specs/2026-08-09-phase-8b-linear-scan-core-design.md`
- [ ] 🔴 `active` list kept **sorted by END point** — the invariant that makes expiry a cheap prefix scan — **note (Phase 8b):** built — `LinearScan::active: Vec<usize>` (indices into `intervals`), re-sorted by `end` after every insertion in both `run()`'s success arm and `evict_and_assign`. `expire_old_intervals` relies on it for its early `break`. Guarded two ways, and the second one only exists because of a review finding worth recording: the corpus-wide invariant test (`active_stays_sorted_by_end_throughout_every_corpus_run`) checks the invariant at three points per iteration — after expiry, after `pick_register`'s Case 2 `active` mutation, and after assignment — but it does so against a *reimplementation* of `run()`'s loop body, so it could not have caught a mutation to `run()` itself. Mutation testing during Task 5's own review surfaced exactly that gap: deleting `run()`'s `self.active.sort_by_key(...)` left every test green. Fixed by adding a real `debug_assert_active_sorted()` call inside `run()` (after expiry, and after the push-and-sort), which kills that mutant and the analogous one in `evict_and_assign` (whose `continue` sends the unsorted `active` straight into the *next* iteration's post-expiry assertion). Same design doc.
- [ ] 🔴 `expire_old_intervals(current_start)` freeing registers — **note (Phase 8b):** built, with a corrected boundary condition. SPEC.md §7's and PROMPT.md's sketches both break on `intervals[j].end > current_start`, which is only right for HALF-OPEN `[start, end)` ranges; 8a corrected `Interval` to INCLUSIVE `[start, end]` (a value is still live AT its `end`), so the shipped code breaks on `end >= current_start` and expires only on `end < current_start`. Transcribing the sketch literally would have freed a register one position early and handed it to a value that genuinely still needed it — a silent wrong-answer bug, not a crash. Pinned by an explicit boundary test (`[0,2]` and `[2,4]` touching at position 2 must stay simultaneously active), which the design doc calls the single most important test in the slice. Frees only `Location::Reg` occupants, never a `Spill` (which never enters `free_regs`, and which 8b never constructs anyway). Same design doc.
- [ ] 🔴 `pick_register` preferring the hint (coalescing) then any free register — **note (Phase 8b):** built, and the "preferring the hint" half of this bullet's own wording turned out to be the hardest thing in the slice — the obvious reading of it is a real, structural bug. The design's first rule was the natural one: honor the hint if the target's register is in `free_regs`. Execution against the real corpus measured that rule honoring **0 of 81 hints, ever, on any program** — a direct consequence of 8a's inclusive-range correction, since a two-address hint's target (`lhs` of `dst = Add(lhs, rhs)`) has `lhs.end == dst.start` by construction, so `lhs` is *always* still active and its register is *never* free when `dst` is processed (and a φ-group anchor shares an identical range, same conclusion). The shipped rule has two cases: **Case 1** — target's register genuinely free — which is kept for defensiveness but is **structurally dead code** against every hint shape `build_intervals` can currently produce, and is therefore tested with a hand-built fixture rather than corpus data; and **Case 2** — target still nominally active but `target.end == this.start` — the legitimate same-instruction two-address handoff, which transfers register ownership directly from the target's `active` entry to this one without the register ever passing through `free_regs`. Case 2 is what actually fires. Measured on the shipped code over the 18-program corpus: **~54% of hints honored** (the test asserts a ≥40% floor, and asserts non-vacuously that hints exist at all — a "no violations" property alone would have passed against the broken zero-hints version). The remaining ~46% are correctly refused: a `lhs` that genuinely outlives `dst`'s start, or a φ-group member simultaneously live with its anchor — per 8a's own design those are expected, not bugs, and their resolution is the deferred final-emission task's parallel-copy insertion. **Load-bearing consequence for CHECKLIST bullet 17 (the independent verifier) and for 8d:** Case 2 makes two intervals that *overlap* under the plain inclusive predicate share one register, deliberately and correctly. The naive property "no two overlapping intervals share a register" is therefore FALSE of this allocator's intended output; the correct statement, which 8d's verifier must implement independently, is that sharing is a violation unless the ranges are disjoint OR they touch at exactly one point that is a real handoff (`a.end == b.start && b.hint == Some(a.value)`, or symmetrically). Exclusions from 8a's `excluded_registers` are honored ahead of the hint in both cases — hand-verified during final review on `1000 / (((x >> 1) + 1) + 1)`, where an `x → x>>1 → +1` chain correctly holds `Rax` throughout while the chain's last member (the `idiv` divisor, excluded from `Rax`/`Rdx`) correctly falls past *both* excluded registers to `Rbx` without dropping its hint target out of `active` or breaking the chain for the earlier members. Because 8b has no interval splitting, 8a's per-*position* exclusions are unioned up to whole-*interval* scope (`precompute_excluded`) before being used as a candidate filter — necessary and sufficient, since every exclusion position is inside its own value's range by construction. **Recorded as still-open, not silently handled:** `Shl`/`Shr`/`Sar`'s `rhs` (the shift amount) has the same hardware shape as `idiv`'s divisor — x86 requires it in `Cl`/`Rcx` — and `excluded_registers` does not cover it, by design. It is genuinely a different case: a shift does not *destroy* its `rhs` the way `cqo`/`idiv` destroys the dividend, so it is fixable by an emission-time `mov` into `cl` (8a's already-accepted sub-problem 1), not by an allocation-time exclusion. But the fixup is bigger than one `mov`: it clobbers whatever else occupies `Rcx`, which the emitter must displace and restore. That is not hypothetical — final review's own hand-verification program `1000 / (((x >> 1) + 1) + 1)` produces exactly it, with the shift amount in `Rdx` and the still-live dividend sitting in `Rcx`. Belongs to the emission-pipeline task; carried here so it is not lost between slices. Same design doc.
- [ ] 🔴 **`fixed` registers are non-negotiable** — ABI argument positions and `idiv`'s implicit rax/rdx force eviction of whoever holds them — **note (Phase 8a):** 8a's contribution to this bullet is a NEGATIVE finding, recorded here because it is load-bearing for 8b/8c and would be misleading to hide behind a cleaner-looking status: **as of Phase 8a, NOTHING populates `Interval::fixed` — it is always `None`, deliberately.** The first implementation (commit `75f36ea`) did exactly what this bullet's wording describes, pinning a `Param`'s ABI register and `IntDiv`/`IntRem`'s `dst` to rax/rdx for the value's WHOLE `[start, end]` range. Post-implementation review found that this produces genuinely UNSATISFIABLE constraint sets on trivial, ordinary programs — confirmed by running the shipped code, not by argument: `a/b + c/d` pins two overlapping quotients to rax forever, and `((a >> 1) % (b >> 1)) + (c >> 1)` pins a 3rd int param (rdx) and an `IntRem` result (also rdx) to the same register over overlapping ranges. Root cause: none of these is actually a whole-lifetime requirement. Each holds for exactly ONE instruction — the `Param`'s own position, or the `idiv`'s own position — after which the value is an ordinary virtual register with no hardware constraint. `Option<PhysReg>` on an `Interval` can only express "always", never "at this one instruction"; the textbook fix is interval SPLITTING, which this project's linear-scan design has no mechanism for. Corrected across commits `89a2182`/`9dc2110`/`b15c576`. What ships instead, per the design doc's rewritten "Fixed registers" section: (a) `Param` and `IntDiv`/`IntRem`'s `dst` get NO interval-level marking at all — the deferred final-emission task recomputes the ABI/hardware register directly from the `MachineInst` and `func.params` (zero `Interval` data needed) and inserts a copy unless the locations already coincide, exactly like the existing dividend-into-rax and two-address fixups; (b) `IntDiv`/`IntRem`'s `rhs` (the divisor) is the ONE genuinely allocation-time constraint — `cqo`/`idiv` destroys it before any copy could run — and is exposed via a separate `excluded_registers(func, selected) -> HashMap<(usize, Value), Vec<PhysReg>>` side channel in `crates/forge-regalloc/src/intervals.rs`, keyed at the `idiv`'s instruction position specifically, NOT over the divisor's whole lifetime. Consequences to carry forward: SPEC.md §7's `LinearScan::run` sketch has an `if let Some(phys) = self.intervals[i].fixed { self.evict_and_assign(...) }` branch that is DEAD as of 8a (the field stays in the struct because a genuinely whole-lifetime hardware constraint may appear in some future `MachineInst` variant); 8b must consume `excluded_registers` as a candidate-set filter and, since it has no splitting, must widen a point exclusion to the whole interval of the affected value; and the idiv third-party-clobber case (an unrelated value sitting in the other of rax/rdx at that program point) is still unresolved and deferred to final emission, which requires 8b/8c to retain full `Interval` + `Value -> Location` data rather than a flattened table. This bullet stays open: the eviction machinery it names is unbuilt, and the emission-time copies that replace it belong to the not-yet-started emission-pipeline task. Details: `docs/superpowers/specs/2026-08-09-phase-8a-liveness-intervals-design.md` — **note (Phase 8b):** partially built, and deliberately narrower than this bullet's wording. `evict_and_assign(i, phys)` exists in `crates/forge-regalloc/src/linear_scan.rs` and `run()` dispatches to it whenever `Interval::fixed` is `Some(_)` — but it handles **ONLY the no-victim case** (nobody currently holds `phys`): it removes `phys` from `free_regs`, assigns it, pushes onto `active` and re-sorts. **The victim-reassignment case this bullet actually names — "force eviction of whoever holds them" — is NOT built**; it hits an explicit, clearly-messaged `unimplemented!()` (covered by a `#[should_panic]` test that pins the message, not just that *some* panic occurs). That is a deliberate scope decision, not an oversight: an earlier design draft *did* write the reassignment path and execution-based review proved it wrong in two independent ways — (1) the victim was pulled out of `active` and given a new register but never re-inserted and never removed from `free_regs`, a genuine double-booked-register bug on the very next interval; and (2) even with that leak fixed, choosing the victim's replacement out of the CURRENT `free_regs` snapshot is unsound, because `free_regs` describes availability at the current scan position, not across the victim's own `[start, end]` — 8a's bug class exactly (a point-in-time fact used as a lifetime property). With `Interval::fixed` still having no real producer (8a's finding above), there is nothing to correctness-test a reassignment strategy against, so it is deferred to 8c, which will have the spill machinery that is the likely correct answer (treat the victim as a spill). Two further known narrownesses, acceptable only because nothing constructs the offending shapes: the no-victim path neither consults `excluded_at` nor checks that `phys` belongs to the interval's own class pool, so a hand-built fixture could get a silently-wrong assignment rather than a panic. This bullet stays open. — **note (Phase 8c):** 8b's note above deferred `evict_and_assign`'s victim-reassignment case to 8c on the theory that 8c's spill machinery would be the right answer. **8c built that machinery and deliberately did NOT wire it in** — recorded here because the previous note promised otherwise, and a reader checking whether the promise was kept deserves a straight answer rather than silence. The reasoning is unchanged from why it was deferred in the first place: `Interval::fixed` still has no real producer (nothing in `build_intervals` sets it, confirmed again at the end of 8c), so there is still nothing to correctness-test a reassignment strategy *against*. Changing the existing `#[should_panic]` test's expectation to "now calls `spill()`" without any real caller producing this shape would be speculative generality dressed as progress, not evidence the path works. It stays a loud, clearly-messaged `unimplemented!()` (message updated in 8c so it no longer reads as though 8c hasn't happened yet), revisited only when a `MachineInst` variant or ABI concern actually produces a `fixed` interval. This bullet stays open.
- [ ] 🔴 `spill_at_interval`: pick the victim — **note (Phase 8c):** built — `LinearScan::spill_at_interval` (`crates/forge-regalloc/src/linear_scan.rs`) replaces 8b's `unimplemented!()` stub. Picks the worst-scoring ACTIVE interval of the same class, then either spills that victim and hands `i` its register (when `victim.end > i.end`) or spills `i` itself. Three corrections to SPEC.md §7's own sketch, none of them cosmetic: (a) the sketch never maintains `active` at all — it neither removes the victim nor inserts `i` nor re-sorts, so the very next `expire_old_intervals` prefix scan would work off a stale, unsorted list; (b) the tie is broken toward spilling `i` (strict `>`, not `>=`) — disturbing a victim that dies at exactly the same position buys nothing; and (c) **the victim's register is NOT automatically legal for `i`** — `i` may carry its own per-instruction exclusion (an `IntDiv`/`IntRem` `rhs` excluded from `Rax`/`Rdx` per 8a), so the reassignment branch consults `excluded_at` first and falls back to spilling `i` when the freed register is excluded. That last one (labelled B6 in this slice's design review) is the finding most worth carrying forward: it is invisible to any test that does not deliberately combine register pressure with a per-instruction exclusion, and both of its ingredients were built by *different* tasks in this slice, so no single-task test could have produced it. Confirmed end-to-end during final review on a hand-built high-pressure IR function with a real `IntDiv` — the excluded divisor is spilled rather than handed the victim's `Rax` — and confirmed load-bearing by mutation (neutralizing the check makes the divisor land in `Rax`, a silently wrong allocation, not a crash). The `.expect("no active interval to spill")` is a real invariant assertion, not defensive noise: an empty `active` after `pick_register` returned `None` would mean the class's whole pool is excluded for `i` specifically, which is unreachable today and tested via `#[should_panic]`. Details: `docs/superpowers/specs/2026-08-09-phase-8c-spilling-design.md`
- [ ] 🔴 **Spill heuristic: furthest endpoint, weighted by use density.** The textbook picks furthest endpoint; weighting by `uses/length` measurably beats it on expression trees, where a value used 4× in a tight window must not be spilled — **note (Phase 8c):** built — `populate_spill_weights(selected, &mut intervals)` computes `uses / length` per VALUE (reads counted with 8a's existing `reads_of`, length floored at 1 so a single-point interval cannot divide by zero), and `spill_at_interval` scores victims as `end / spill_weight.max(0.01)`. Called once, up front, inside `allocate()` *before* class-partitioning rather than lazily at the comparison site, so the two branches of the heuristic can never drift apart — the same "one shared pure function, not two independent implementations" rule Phase 7b's `match_scaled_index` established. This is the field 8a deliberately shipped as always-`0.0`. Weights are per value, never per φ-group: a φ's members share one range by construction but *not* one use count, and spilling a whole group together is not even representable once the merge has produced N independent `Interval`s. **Recorded honestly because it is the kind of gap that survives a review round:** the test that was supposed to prove the `uses/length` weighting actually drives victim selection was initially VACUOUS — it still passed with the production formula neutralized to `end`-only, because its two candidates had equal `end`s and `max_by`'s tie-breaking did the work. It had to be rewritten with genuinely opposing values (a long-lived, heavily-used interval scoring 200/100.0 = 2.0 against a short-lived, barely-used one scoring 50/0.01 = 5000.0) before it proved anything at all. Same lesson as 8b's `active`-sorted mutant: a test asserting the right *outcome* is not the same as a test that can only pass for the right *reason*. Same design doc.
- [ ] 🔴 Spill slot allocation on the stack frame, with slot reuse after an interval ends — **note (Phase 8c):** built, and the mechanism is deliberately NOT the one this bullet's wording ("reuse after an interval ends") suggests. `LinearScan` gained a single `slot_end: Vec<u32>` field — no `spilled` list, no `free_slots` stack, no `next_slot` counter — where `slot_end[s]` is the highest `end` any interval ever placed in slot `s`, and `spill(i)` reuses slot `s` iff `slot_end[s] < i.start`: the same inclusive-boundary rule `expire_old_intervals` uses for registers, but phrased against **the requesting interval's own start rather than a scan cursor**, which makes it order-independent and removes the need for any expiry step. The cursor-relative design came first and was proven wrong by execution in two independent ways, both of them the point-in-time-vs-lifetime bug class this project has now hit five times (8a's whole-lifetime `fixed` pin, 8b's naive hint-interference check, 8b's `evict_and_assign` replacement-register choice, and both of these): (1) `spill_at_interval` can spill a victim whose `start` is far BEHIND the scan cursor, so `free_slots.pop()` returns a slot that is free *now* but not across the victim's own range — `X([0,6])` and `G([5,300])` genuinely overlap yet both landed on slot 0; and (2) threading the freed-slot stack between the GPR and XMM passes let a slot freed relative to the GPR cursor be handed to the XMM pass restarting at 0, colliding a GPR and an XMM spill that were simultaneously live. Worth recording flatly: **this slice's design doc opens with an explicit warning about exactly this bug class, and its own first draft of this exact section still committed it, twice.** Both failures are now pinned as named regression tests. Slot numbering is GLOBAL across both class passes (`slot_end` threaded forward through `allocate()`, never reset), because a slot index is a byte-offset multiplier and both classes spill 8 bytes — resetting per class produces two individually-correct-looking passes and one corrupt combined allocation, which no single-class test can catch. Same design doc.
- [ ] 🔴 Insert reload before each use of a spilled value, store after its definition — **note (Phase 8c):** NOT built — this slice built only the *precondition* that makes it buildable, and stops at the same MachineInst-vs-real-bytes boundary as every Phase 7/8 sub-slice. What ships: `SCRATCH_GPR = [R10, R11]` and `SCRATCH_XMM = [Xmm14, Xmm15]`, two registers per class reserved exclusively for reload/store traffic and removed from the pool the allocator hands out (`SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM`, 12 and 14 registers, which is what `allocate()` now scans against). The point of the reservation is to kill the recursion textbook linear scan has here: if each reload were its own tiny interval competing for a register, satisfying it could require spilling something else, with no natural termination proof. A compile-time-fixed, never-allocatable register cannot be contended for at all — `lhs` (or a unary op's sole operand) always reloads into `SCRATCH_*[0]`, `rhs` into `SCRATCH_*[1]`, no per-position bookkeeping, and the 2-address `dst`-reuses-`lhs` convention composes for free. Two per class is sufficient because every `MachineInst` reads at most two values today (`CallLibm` is the only variadic-shaped variant and `LibFunc`'s widest member, `Pow`, takes 2) — re-check this if a 3-operand variant ever lands. `R10`/`R11` rather than the originally-drafted `R14`/`R15` because the latter are both in `prologue::SYSV_CALLEE_SAVED`, which would have made every spilling function push/pop a pair of registers used only transiently; all XMM registers are caller-saved under System V so there is no equivalent choice to get wrong on that side. Still owed by the emission task, and deliberately so: the actual `mov` instructions, the interaction with `idiv`'s third-party clobber (displacing whatever occupies `Rax`/`Rdx` is a *separate* need from reloading an operand — resolvable with ordinary `push`/`pop`, not by contending for the same two scratch registers), and the rule that a reload/store sequence must not straddle a `CallLibm` without re-reloading afterward. Same design doc.
- [ ] 🔴 Register hints from two-address fixups and from φ operands (coalescing) — **note (Phase 8a):** built — both sources populated into `Interval::hint: Option<Value>` at construction time rather than left for 8b. Two-address hints are copied from `SelectedFunction::coalescing_hints` (already fully computed in Phase 7b). φ-operand hints turned out to need real design work, and this bullet went through three separate corrections that are worth recording, since each was a silent-wrong-answer bug rather than a crash: (1) the φ RANGE MERGE ALONE IS NOT SUFFICIENT — a merged φ-group is N separate `Interval`s sharing one identical range, which to an allocator with no other signal looks like N mutually-interfering values needing N DIFFERENT registers, the exact opposite of what φ-coalescing needs; fixed by giving every group member a mutual hint. (2) The anchor must be the group's SMALLEST `Value` number, NOT the φ's own destination — after the merge all members share a range, so 8b's `(start, end, value)` scan order tie-breaks on raw `Value` number, and a φ is minted AFTER the values it merges, so anchoring on the φ pointed every hint FORWARD at a not-yet-assigned interval. (3) Even with a `hint.is_none()` guard giving φ hints precedence over two-address hints (φ-coalescing has no cheap emission-time fallback yet; a two-address mismatch is always fixable by one `mov`), a two-address hint could STILL point forward, because the φ merge widens a member's `start` backward past its own `lhs`'s definition when that `lhs` lives in the other if/else arm; fixed by additionally requiring the hinted value's scan key to sort strictly before the destination's, dropping the hint otherwise. Three forward-hint bugs each survived multiple fixture-based review rounds, so the invariant is now guarded by a corpus-wide PROPERTY test (`every_hint_points_backward_in_8bs_scan_order`) over 18 programs including float/XMM-side counterexamples, with a non-vacuousness assertion so it cannot pass trivially if hint population breaks entirely. Note for 8b: a hint is a preference among interference-free candidates and must NEVER be honored without an interference check first — nested `if`s routinely produce φ-groups whose members provably cannot share a register even at zero register pressure (in `if a > b then (if a > c then a else c) else b`, `a` and `b` are in one merged component yet are read simultaneously by the `Cmp` deciding the outer branch), which makes parallel-copy insertion at predecessor block ends a ROUTINE emission requirement, not a rare pressure-driven fallback. Details: same design doc.
- [ ] 🔴 Separate allocation for GPR and XMM classes — **note (Phase 8b):** built — `allocate(intervals, excluded_registers) -> HashMap<Value, Location>` (`crates/forge-regalloc/src/linear_scan.rs`, re-exported from `forge_regalloc`) partitions the interval list by `RegClass` and runs the identical single-class scan loop twice, each with its own `active`/`free_regs` seeded from `ALLOCATABLE_GPR` (14 registers: all 16 minus `Rsp` and `Rbp`) or `ALLOCATABLE_XMM` (16: `Xmm0`-`Xmm15` only — `Xmm16`-`Xmm31` need EVEX, which nothing in this codebase can encode, so handing one out would produce unencodable bytes), then merges both maps. This is the decomposition doc's stated default rather than one interleaved scan with per-class pools; the two are equivalent here because **no hint or φ-group can ever cross the class boundary**, justified from both φ-producing paths rather than just the obvious one: `forge-syntax`'s typeck rejects mismatched `if`/`else` arm types before lowering, AND `forge-ir`'s `Builder::new_phi` carries the variable's single declared `Ty` through `fill_phi_operands` by construction — plus every arithmetic `MachineInst`'s operands and result share one `Ty`. Worth recording for 8c/8d: the partition step is the kind of thing that passes every property test while being silently deleted (a merged single-pool run still produces a no-overlap-clean, fully-assigned map), so it has its own dedicated assertion that every `Xmm`-class value lands in `ALLOCATABLE_XMM` and every `Gpr`-class value in `ALLOCATABLE_GPR` — added during Task 5's review after mutation testing found the gap, not during the original test-writing pass. Details: `docs/superpowers/specs/2026-08-09-phase-8b-linear-scan-core-design.md` — **note (Phase 8c):** `allocate`'s signature above is now stale in two ways, both deliberate. It is `allocate(intervals, excluded_registers, selected: &SelectedFunction) -> (HashMap<Value, Location>, u32)`: the new `selected` parameter feeds `populate_spill_weights` (nothing before 8c needed the `MachineInst` stream itself, only the intervals derived from it), and the new `u32` return is the total spill-frame byte count — slot count × 8, unpadded — which is **the first real producer of the value `prologue::emit_prologue`/`emit_epilogue`'s `spill_bytes` parameter has been waiting for since Phase 7d**. Verified against that consumer during 8c's final review rather than assumed: `emit_prologue` documents `spill_bytes` as the RAW request and pads it to 16-byte alignment internally via `padded_spill_bytes`, so `allocate`'s multiple-of-8 count is exactly the right thing to hand it and must NOT be pre-padded. The two per-class passes also now scan `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM` (12/14) instead of the raw `ALLOCATABLE_GPR`/`_XMM` (14/16) — the reservation described in the reload/store bullet above — and thread one shared `slot_end` between them. The pool shrink was re-confirmed to have zero observable effect on the existing corpus (max simultaneous liveness 4 GPR / 7 XMM), and the corpus test that asserts this now also asserts the returned byte count is `0`, so a corpus program that starts needing a spill fails loudly instead of passing quietly.
- [ ] 🔴 **Independent allocation verifier**: no two overlapping intervals share a register. Written separately from the allocator so it can't share a bug with it — **note (Phase 8d):** built — `verify_allocation(intervals, assignment) -> Result<(), String>` in the new `crates/forge-regalloc/src/verify.rs`, re-exported from `forge_regalloc`. The independence this bullet asks for is STRUCTURAL, not promised in a comment: `verify.rs`'s only non-test reference to the allocator is `use crate::linear_scan::Location` — a type definition, no function, no shared helper, checkable by grep. The property is re-derived from the raw `Interval`/assignment data by a plain pairwise scan. **This bullet's own wording is wrong as literally stated, and so is PROMPT.md's sketch of it — in TWO ways that CANCEL, which is why neither had been caught before.** Measured against the real 18-program corpus: PROMPT.md's literal sketch (half-open `a.start < b.end && b.start < a.end` AND no handoff exemption) rejects **0/18**, and its real failure mode is a FALSE NEGATIVE, not the over-strictness one would expect — the half-open test is exactly false for a touching pair (`[0,2]` vs `[2,4]`: `2 < 2` fails), so it silently ACCEPTS a genuine double-booking at a touching, non-handoff position without ever reaching the exemption question. The half-fixed variant (inclusive predicate, exemption still missing) is the over-strict one, and rejects **17/18**. Fixing either defect alone is strictly worse than fixing neither. The shipped property: sharing a register is a violation UNLESS the ranges are disjoint OR they touch at exactly one point that is a real hinted handoff (`a.end == b.start && b.hint == Some(a.value)`, or symmetrically) — `pick_register`'s Case 2 hands one register to two touching intervals on purpose, and that is coalescing working, not a conflict. Extended beyond this bullet's literal "share a register" to `Location::Spill` as well, with **no** exemption on that side: a stack slot has no same-instruction handoff mechanism, so copy-pasting the register exemption onto it would be a real bug (pinned by its own test). That branch is unreachable from today's `spill()`, whose `slot_end[s] < start` reuse test is already strict — kept anyway, because leaning on the allocator's own invariant is precisely the "shares a bug with the thing it checks" failure this bullet exists to prevent. **Recorded honestly as a known limitation, not a hypothetical:** this verifier would REJECT a *correct* φ-coalesced allocation. `merge_phi_intervals` (8a) gives every φ-group member an IDENTICAL range plus a mutual hint at the anchor, and identical ranges fail the exemption's `a.end == b.start` precondition — so two φ members sharing one register read as a conflict even though honoring the hint there is entirely correct. Unreachable today *only* because `pick_register` structurally cannot honor a φ hint at all (Case 1 needs the target's register already free, never true for an identical-range pair; Case 2 needs `target.end == this.start`, false when both ends are equal); re-measured at this slice's final whole-phase review, 10 φ-shaped identical-range hinted pairs exist across the corpus and zero are co-located. The first time φ coalescing is made to work, this verifier — meant to run on every debug compilation — will reject the correct code it produces. Not pre-fixed: there is no real producer to test an exemption against, and building one would be exactly the speculative generality this project avoids elsewhere. SPEC.md's Phase 8b note was also corrected here: its claim that "a φ-group pair can never satisfy the exemption — a merged group's range structurally spans at least two positions" is unsound as reasoning (a zero-length pair satisfies `a.end == b.start` without two distinct positions) and points the wrong way (the exemption's risk is being too NARROW, not too wide). Deliberately narrower than `linear_scan.rs`'s own tests in two ways that fall out of the `match`'s catch-all arm: a `Value` missing from `assignment` entirely, and a class/pool mismatch, are both already covered there and are properties of `allocate()`'s contract rather than of interval overlap. NOT wired into "run on every compilation in debug builds" — that's Phase 11's, per this file's own phase split. **One more scope caveat, found by the whole-phase review and worth stating loudly because the bullet's own name invites the wrong reading: `Ok(())` from this function means "no two overlapping intervals share a location," NOT "this allocation is correct."** The gap is live today, not theoretical: nothing in this allocator models CALL CLOBBERS (`excluded_registers` covers only `IntDiv`/`IntRem`'s `Rax`/`Rdx`), and every XMM register is caller-saved under System V, so a float value living across a `CallLibm` is destroyed by it. Measured on SPEC.md's own canonical example of this, `sin(x) + cos(y)`: `Value(1)` is live across the `CallLibm` at position 2 and is assigned `Xmm1`, which that call destroys — and this verifier returns `Ok` on it, correctly by its own contract, because interval overlap is not the property being violated. That is bullet 22's job below (Phase 8e) to make the allocator handle and to test. Details: `docs/superpowers/specs/2026-08-09-phase-8d-verification-reporting-design.md`
- [ ] 🔴 Report register pressure per program point (drives the workbench panel) — **note (Phase 8d):** built — `register_pressure(intervals, program_length) -> Vec<PressurePoint>` with `PressurePoint { gpr: u32, xmm: u32 }`, in the new `crates/forge-regalloc/src/pressure.rs`, re-exported. A standard sweep-line (+1 at `start`, -1 at `end + 1` for INCLUSIVE ranges, then a prefix sum), DENSE — one entry per position in `0..program_length`, not a sparse step function — because the stated consumer is a chart plotting pressure against the instruction-index axis, and a sparse form just pushes the same expansion onto every caller. Split by class rather than one combined count: GPR and XMM come from wholly separate pools (8a-8c), so a combined number would conflate two counts that are never compared against the same budget. `program_length` is a required parameter, deliberately not inferred from `intervals.iter().map(|iv| iv.end).max()` — a function's real instruction count can legitimately exceed every interval's `end`, and under-sizing the report would drop those trailing positions instead of reporting them as the zero pressure they are; callers pass `selected.insts.len()`. The `end_exclusive` clamp keeps the function total (no panic, no `u32` wraparound from an unmatched decrement) when a caller hands in a `program_length` inconsistent with the intervals, rather than correct-only-when-inputs-agree. **The whole-phase finding worth carrying forward, because it is invisible from either bullet alone: peak pressure is an UPPER BOUND on register demand, not equal to it.** This function counts simultaneously-live intervals, but `pick_register`'s Case 2 deliberately puts two live intervals in ONE register at a touching position — the exact same handoff the verifier above had to carve an exemption for, seen from the other side. Measured at final review on `if a > b then (a * c) + (b * c) else a - b`: peak 7 simultaneously-live XMM values, but only 6 distinct XMM registers ever occupied. So the workbench panel this bullet describes — the pressure curve with a red line at the machine register count — will overstate demand by exactly the number of live handoffs at each position, and should not be read as "this many registers are needed here." The corpus test asserts `peak <= SPILL_AWARE_ALLOCATABLE_*` pool size (12/14, the pools `allocate()` actually scans since 8c — NOT the wider 14/16 `ALLOCATABLE_*`) plus that program's spill count; on this corpus nothing spills, so the bound is slack there, but it was confirmed TIGHT at final review on a synthetic high-pressure allocation (20 simultaneously-live GPR intervals = 12 pool + 8 spills exactly; 20 XMM = 14 + 6 exactly), which is what makes it a real cross-check against the allocator rather than an unfalsifiable assertion. The workbench panel itself is NOT built — the frontend is a separate, not-yet-started project — so this slice's job ends at producing the data. Same design doc.
- [ ] 🔴 Test: 3 values, 16 registers → no spills — **note (Phase 8e):** built — `bullet_19_three_values_no_spills` in the new `crates/forge-regalloc/tests/integration.rs` (an external, public-API-only test crate). "16 registers" is stale wording from before Phase 8c introduced `SCRATCH_GPR`/`SCRATCH_XMM` reservation; the real pool this test runs against is `SPILL_AWARE_ALLOCATABLE_GPR` (12). A real 3-*variable* source program cannot produce exactly 3 values (3 `Param`s plus at least 1 combining op is always at least 4 values, and untyped surface arithmetic lowers to F64/XMM anyway, not GPR) — confirmed by execution during design review, not assumed. The test instead hand-builds a 2-`Param`-plus-1-`Add` `Ty::I64` function via `forge_ir::builder::Builder`, which goes through the exact same real `select`/`build_intervals`/`allocate` pipeline; only the front-end source-text stage is bypassed. Details: `docs/superpowers/specs/2026-08-10-phase-8e-integration-tests-benchmark-design.md`
- [ ] 🔴 Test: 40 simultaneously live values, 16 registers → correct results with spills — **note (Phase 8e):** built, narrower than literally worded, and the narrowing is deliberate — `bullet_20_forty_live_values_forces_spilling_and_stays_valid` in `crates/forge-regalloc/tests/integration.rs`. "Correct RESULTS" can only mean one thing in a compiler context: run the compiled code and check its output against a known-good answer. That needs the not-yet-built `MachineInst`-to-bytes emission pipeline (task #68 — `crates/forge-x64/src/assembler.rs` has zero references to `MachineInst` as of this writing) — the execution HARNESS itself is not the blocker, `crates/forge-mem` already ships a complete one (`ExecutableBuffer`, `CompiledExpr`, `CodeCache`, 14 passing tests); only task #68 is missing. What's built instead: 40 hand-built, maximally-overlapping GPR intervals against the real `SPILL_AWARE_ALLOCATABLE_GPR` pool (12) force exactly 28 spills and exactly 224 frame bytes (both exact, not approximate — no two spilled values can ever reuse a slot since every interval shares one identical range), and the resulting allocation is independently confirmed valid via `verify_allocation`. On this specific fixture (every interval `hint: None`, so the handoff exemption never fires) that `Ok` is a real but narrow regression guard — it can only fail if `allocate()` double-books a register outright, not a general correctness proof. Details: same design doc.
- [ ] 🔴 Test: verifier catches a deliberately broken allocation — **note (Phase 8e):** already satisfied — no new code. `verify.rs`'s `catches_a_deliberately_broken_allocation` (Phase 8d) does exactly this: takes a real corpus program's real allocation, force-reassigns a genuinely-overlapping non-handoff pair onto the same location, confirms `verify_allocation` returns `Err`, with a `checked > 0` non-vacuousness guard (every one of the 18 corpus programs actually contains such a pair). Both the `Location::Reg` and `Location::Spill` branches are covered — the latter by two dedicated hand-built fixtures in the same file, since the corpus itself never spills.
- [ ] 🔴 Test: expression calling libm → caller-saved values are spilled around the call — **note (Phase 8e):** built, narrower than literally worded, and the narrowing is deliberate — `bullet_22_libm_call_clobber_hazard_is_real_and_currently_unverified` in `crates/forge-regalloc/tests/integration.rs`. "Spilled around the call" describes an EMISSION-time save/restore sequence — the exact same category of problem as `idiv`'s third-party `Rax`/`Rdx` clobber (Phase 8c's design doc: "resolvable at emission time via ordinary stack `push`/`pop` for the displaced occupants"), deferred to the same not-yet-built emission pipeline (task #68) as bullet 20. What's built and worth having now: compile `sin(x) + cos(y) + x + y` through the real pipeline, confirm `verify_allocation` returns `Ok` (the documented scope boundary from bullet 17's note above — this allocator does not model call clobbers, and this is not a test failure), AND confirm at least one XMM interval's range STRICTLY contains a real `CallLibm`'s position (`iv.start < pos && pos < iv.end`) — the non-vacuousness check proving the hazard is real on this program, not hypothetical. The strict predicate matters: an earlier draft used the inclusive `<=`/`<=` form, which is trivially satisfiable by any libm call's own argument/result intervals with ZERO genuine cross-call liveness (`sin(x)` alone scores 2 hits under the inclusive form, 0 under the strict one — confirmed by execution). Same design doc.
- [ ] 🔴 Test: coalescing eliminates redundant `mov` for a two-address chain — **note (Phase 8e):** already satisfied — no new test code, one stale-comment fix. `linear_scan.rs`'s `run_allocates_a_straight_line_chain_via_transfers` (Phase 8b) confirms `x = param; one = 1; a = x + one; c = a + one` allocates `x`/`a`/`c` (the genuinely chained values, two successive `pick_register` Case 2 handoffs) to ONE shared physical register — the allocation-level precondition for eliminating a `mov` at each handoff. Literally checking the `mov` itself is elided needs the not-yet-built emission pipeline, same boundary as bullets 20/22. The test's own doc comment was factually wrong (claimed three `Add`s and "all four values" sharing a register; the real fixture has two `Add`s, and the fourth value, `one`, correctly does NOT share the chain's register) — corrected in this phase after the error was traced as the source of an identical mistake an earlier draft of this phase's own design doc copied from it.
- [ ] 🟡 Benchmark: allocation of 1000 values < 50 µs — **note (Phase 8e):** built — the first `criterion` benchmark anywhere in this workspace (`criterion = "0.5"` was a workspace dependency with no consumer until now), `crates/forge-regalloc/benches/allocation.rs`, 1000 staggered short-lived intervals (NOT one maximally-overlapping block — that shape belongs to bullet 20's correctness stress test, not a throughput benchmark), split evenly GPR/XMM. **Target NOT met, recorded honestly rather than declared satisfied.** Baseline measurement (`std::collections::HashMap`/`HashSet` throughout `LinearScan`): 174.51 µs, roughly 3.5x over target — confirming an independent measurement from design review (130-145 µs on a different machine) was not a fluke. A real, scoped fix was applied and shipped in this same phase: `LinearScan`'s three internal hot-path containers (`free_regs`, `assignment`, `excluded`) swapped from `std`'s SipHash-based `HashMap`/`HashSet` to `rustc_hash::FxHashMap`/`FxHashSet` (already a workspace dependency, already used by `forge-ir`/`forge-opt`/`forge-syntax`, not previously by `forge-regalloc`) — a mechanical, behavior-preserving swap scoped to exactly those three fields and their feeders, explicitly NOT `allocate()`'s public `excluded_registers` parameter (a cross-crate API boundary type) and NOT the ~8 unrelated `HashSet<PhysReg>` usages already in this file's own test module (needed zero changes, since the `rustc_hash` import was added additively alongside the existing `std` one rather than replacing it). Result, measured twice for stability (56.14 µs then 55.22 µs, p=0.80, no significant drift): **~55-56 µs, a 68% reduction from baseline, but still roughly 11-12% over the 50 µs target.** Not force-fit by further tuning or by weakening the benchmark's own workload — this is the real, current number. A further, deeper optimization (e.g. avoiding the `self.intervals[i].clone()` in `pick_register`, or restructuring `active`'s linear removal) would likely close the remaining gap but was judged out of this phase's scope (a benchmark-driven perf fix, not an allocator redesign); left as a known, honest follow-up, the same "recorded, not silently worked around" pattern already used for `evict_and_assign`'s deferred victim case and reload/store insertion. Details: `docs/superpowers/specs/2026-08-10-phase-8e-integration-tests-benchmark-design.md`

---

## Phase 9 — AArch64 Backend (20 tasks)

- [ ] 🟡 `PhysReg` for X0-X30, SP, and V0-V31
- [ ] 🟡 Fixed-width 32-bit instruction emitter: `Vec<u32>`, `to_le_bytes` at the end
- [ ] 🟡 `add`/`sub` shifted-register and immediate forms (12-bit imm with optional LSL 12)
- [ ] 🟡 `mul` `sdiv` `madd` `msub`
- [ ] 🟡 `and` `orr` `eor` `lsl` `lsr` `asr`
- [ ] 🟡 `fadd` `fsub` `fmul` `fdiv` `fsqrt` `fabs` `fneg` (double)
- [ ] 🟡 **`fmadd` / `fmsub` — 3-operand FMA in the base ISA**, where x86 needed a whole new instruction set
- [ ] 🟡 `fcmp` + `fcsel` (branchless select, cleaner than x86's `cmov`+`ucomisd` dance)
- [ ] 🟡 `fmin` `fmax` `frintm`/`frintp`/`frintn`/`frintz` (floor/ceil/round/trunc)
- [ ] 🟡 `scvtf` `fcvtzs`
- [ ] 🟡 `ldr` `str` with the scaled-immediate offset encoding
- [ ] 🟡 `b` `b.cond` `bl` `ret` with 26-bit / 19-bit offsets
- [ ] 🟡 **Immediate encoding: the one genuine pain.** Integers use the bitmask-immediate form (rotated run of ones) or a MOVZ/MOVK sequence of up to 4 instructions
- [ ] 🟡 **Float immediates: only 64 specific values are encodable.** Everything else needs a literal pool load
- [ ] 🟡 `encode_logical_imm(value) -> Option<(N, immr, imms)>`
- [ ] 🟡 AAPCS64: X0-X7 / V0-V7 args, X19-X28 callee-saved, 16-byte stack alignment
- [ ] 🟡 Prologue/epilogue with `stp`/`ldp` pairs
- [ ] 🟡 Round-trip verification via `capstone`
- [ ] 🟡 Test under QEMU on x86 CI
- [ ] 🟡 **Test: same expression, same result on x86-64 and AArch64** (bit-identical)

---

## Phase 10 — SIMD Vectorization (22 tasks)

- [ ] 🟡 `CpuFeatures` via `raw-cpuid`: SSE2, SSE4.1, AVX, AVX2, FMA, AVX-512F/DQ, BMI2
- [ ] 🟡 AArch64: NEON always present; detect SVE if available
- [ ] 🟡 `best_width(ty)` selecting 2/4/8 lanes for f64
- [ ] 🟡 **Runtime selection is a genuine JIT advantage over AOT** — surface this in the workbench
- [ ] 🟡 Array-mode IR: `Load`/`Store` with an induction variable, loop blocks
- [ ] 🟡 Vectorizer: scalar op → lane-wise op, scalar load → `VecLoad`, unroll by width
- [ ] 🟡 `Splat` for loop-invariant scalars
- [ ] 🟡 **Tail handling**: `N % width` leftover elements via a scalar epilogue
- [ ] 🔵 AVX-512 masked tail using k-registers — no epilogue needed at all
- [ ] 🟡 Alignment: use `vmovupd` by default; emit `vmovapd` only when alignment is provable
- [ ] 🟡 Reductions: horizontal sum via `vextractf128` + `vaddpd` + `vhaddpd`
- [ ] 🟡 Packed encoders: `vaddpd` `vmulpd` `vsqrtpd` `vfmadd231pd` at 128/256 bit
- [ ] 🔵 EVEX 512-bit forms
- [ ] 🟡 NEON: `fadd.2d` `fmul.2d` `fmla.2d`
- [ ] 🟡 Loop prologue/epilogue with the induction variable and bounds check
- [ ] 🟡 **Test: SIMD result == scalar result, element for element, including the tail** — run for N = 1..100 to hit every tail length
- [ ] 🟡 Test: unaligned input produces correct results
- [ ] 🟡 Test: reduction matches a sequential sum (note: FP addition isn't associative, so document the expected difference and use an epsilon *only here*)
- [ ] 🟡 Benchmark: 1M-element `a*b+c` at each width
- [ ] 🟡 Benchmark: demonstrate memory-bandwidth saturation — 8-wide is *not* 2× faster than 4-wide, and that's the lesson
- [ ] 🟡 Fallback path when a feature is absent, verified by masking features off
- [ ] 🟡 Test on a CPU without AVX2 (or with features masked) → correct scalar fallback

---

## Phase 11 — Differential Testing & Verification (18 tasks)

**The spine. Wire the first three tasks up in Phase 6.**

- [ ] 🔴 `arb_expression(depth)` proptest strategy generating well-typed random expressions
- [ ] 🔴 **`jit_matches_interpreter`: bit-exact comparison via `to_bits()`**, not approximate equality — without fast-math the JIT must produce *identical* results and any drift is a real bug
- [ ] 🔴 NaN handling: `is_nan()` on both sides rather than bit comparison (NaN payloads may differ legitimately)
- [ ] 🔴 Input generation covering: normal values, ±0, ±Inf, NaN, subnormals, `f64::MIN`/`MAX`
- [ ] 🔴 **Optimization-level equivalence: `-O0` == `-O1` == `-O2`** across the corpus
- [ ] 🔴 Fast-math mode compared with a documented, bounded epsilon
- [ ] 🔴 **Encoding round-trip for every instruction emitter**
- [ ] 🔴 Golden hex tests: expression → exact expected bytes, so encoding changes are deliberate
- [ ] 🟡 Cross-architecture: x86-64 vs AArch64 (QEMU) vs WASM produce identical results
- [ ] 🔴 Register allocation verifier run on every compilation in debug builds
- [ ] 🔴 IR verifier run after every pass in debug builds
- [ ] 🟡 Fuzz target: random bytes → parser → never panics
- [ ] 🟡 Fuzz target: random IR → optimizer → verifier still passes
- [ ] 🔴 Stress test: compile and execute 100,000 random expressions, assert no crashes and no leaks
- [ ] 🔴 Miri run over everything except the actual `transmute`-and-call (which Miri cannot model)
- [ ] 🟡 valgrind for the executable-memory paths — **covered by the Linux Valgrind executable-memory CI job**, which runs the existing 10,000-cycle `forge-mem` test under Memcheck; Miri remains a documented non-goal for raw executable-memory tests.
- [ ] 🔴 Test: expression with 500 live values (forces heavy spilling) still correct
- [ ] 🔴 Test: deeply nested expression (depth 100) compiles and runs

---

## Phase 12 — Tiered Runtime (12 tasks)

- [ ] 🟡 `TieredExpr { ir, tier: AtomicU8, invocations: AtomicU64, baseline: OnceCell, optimized: OnceCell }`
- [ ] 🟡 Thresholds: baseline at 10 invocations, optimized at 1000
- [ ] 🟡 `eval()` incrementing the counter and dispatching to the current tier
- [ ] 🟡 Compilation triggered exactly once per tier via `OnceCell`
- [ ] 🟡 Baseline JIT: skip the optimizer entirely, naive register allocation, fast to compile
- [ ] 🟡 Optimizing JIT: full pipeline + SIMD
- [ ] 🟡 Thread safety: multiple threads may call concurrently; compilation happens once
- [ ] 🟡 Compile-time and per-tier execution-time instrumentation
- [ ] 🟡 **Break-even analysis**: compile cost / (per-call saving) = invocations to profit. Surface this number
- [ ] 🟡 Test: results identical regardless of tier
- [ ] 🟡 Test: tier transitions happen at the right invocation counts
- [ ] 🔵 On-stack replacement for long-running array loops

---

## Phase 13 — CLI (14 tasks)

- [ ] 🔴 `clap` derive with all subcommands from SPEC §16
- [ ] 🔴 `eval EXPR --x 3 --y 4` — parse, compile, run, print
- [ ] 🔴 `ir EXPR [--after PASS]` — textual SSA IR
- [ ] 🔴 **`asm EXPR`** — offset / hex bytes / disassembly, three aligned columns
- [ ] 🔴 `--annotate` on `asm`: per-byte field breakdown (REX bits, ModRM mod/reg/rm decoded)
- [ ] 🔴 `cfg EXPR --dot` — graphviz output
- [ ] 🔴 `regalloc EXPR` — intervals, assignments, spills, peak pressure
- [ ] 🔴 `bench EXPR --sizes 1,10,100,1K,1M` — interpreter vs tiers vs SIMD widths
- [ ] 🔴 `verify EXPR --iters 100000` — differential run with a summary
- [ ] 🔴 `cpuinfo` — detected features and chosen vector width
- [ ] 🔴 `compile EXPR --arch --opt --features` with an `--emit` flag
- [ ] 🟡 REPL with history, persistent variable bindings, `:asm` / `:ir` / `:bench` commands
- [ ] 🔴 Colored output honoring `NO_COLOR`
- [ ] 🔴 Exit codes: 0 ok, 1 runtime error, 2 compile error, 3 verification failure

---

## Phase 14 — WASM Backend & Bindings (14 tasks)

- [ ] 🟡 `forge-wasm`: IR → WASM bytes
- [ ] 🟡 Stack-machine emission via post-order traversal — no register allocation needed
- [ ] 🟡 f64 opcodes: `f64.const/add/sub/mul/div/sqrt/abs/neg/min/max/floor/ceil/nearest/trunc`
- [ ] 🟡 Comparison + `select` for conditionals
- [ ] 🟡 Locals for `let` bindings and spilled values
- [ ] 🟡 Full module encoding: type/function/memory/export sections
- [ ] 🟡 Runtime instantiation via `WebAssembly.instantiate` — a real JIT, in the browser
- [ ] 🔴 `forge-wasm-api` with `wasm-bindgen`
- [ ] 🔴 `parse_and_check(src) -> { ast, diagnostics }`
- [ ] 🔴 `compile(src, opts) -> { ir_stages, cfg, intervals, asm, hex, wasm_bytes }`
- [ ] 🔴 `run(src, args) -> f64` executing the WASM backend
- [ ] 🔴 `benchmark(src, sizes) -> BenchResult` — real timings in the browser
- [ ] 🔴 `cpu_features()` (reports the *simulated* target, since the browser can't see host features)
- [ ] 🔴 `wasm-pack build --target web --release`; `wasm-opt -Oz`; bundle < 1.1 MB gzipped

---

## Phase 15 — Workbench Frontend (34 tasks)

**Foundation**
- [ ] 🔴 WASM init with a loading state; zustand store holding source + all compilation artifacts
- [ ] 🔴 Debounced recompile (200 ms); one pipeline run fans out to all panels
- [ ] 🔴 Resizable panel grid, persisted to localStorage

**Panel 1 — Editor**
- [ ] 🔴 CodeMirror 6 with the expression language; error squiggles from our diagnostics
- [ ] 🔴 Variable binding panel: set values for free variables
- [ ] 🔴 Mode toggle: scalar / array (SIMD)
- [ ] 🟡 Example gallery; share-via-URL

**Panel 2 — AST + SSA IR ⭐**
- [ ] 🔴 D3 tree for the AST
- [ ] 🔴 Textual SSA IR with per-value hover
- [ ] 🔴 **Three-way linking**: hover an IR value → highlights the AST node *and* the source span
- [ ] 🟡 Type annotation on every value

**Panel 3 — Optimization Pipeline ⭐**
- [ ] 🔴 Stepper through every pass, with before/after IR
- [ ] 🔴 **Diff view**: removed instructions red, added green
- [ ] 🔴 Per-pass: which rules fired, instruction count delta, dependency depth delta
- [ ] 🟡 Rule table with `Validity` annotation (Always / IntOnly / FastMathOnly) and an explanation of why
- [ ] 🟡 Fast-math toggle showing which extra rules unlock

**Panel 4 — CFG**
- [ ] 🟡 D3 + dagre graph; blocks with instruction lists
- [ ] 🟡 Edges labeled with branch conditions
- [ ] 🟡 φ-nodes highlighted with edges to incoming blocks

**Panel 5 — Register Allocation ⭐**
- [ ] 🔴 **Live interval chart**: X = instruction index, one bar per value, colored by physical register
- [ ] 🔴 Overlapping bars can never share a color — the constraint made visible
- [ ] 🔴 Spilled values with a hatched pattern; spill/reload points marked
- [ ] 🔴 **Register pressure curve** overlaid, with a red line at the register count — where it crosses is exactly where spills occur
- [ ] 🟡 Hover an interval → highlight the IR value and its uses

**Panel 6 — Assembly + Hex ⭐**
- [ ] 🔴 Three synchronized columns: offset / hex bytes / disassembly
- [ ] 🔴 **Hover a byte → field breakdown**: VEX prefix, opcode, ModRM with mod/reg/rm decoded, displacement, immediate
- [ ] 🔴 Click an instruction → highlight the originating IR value
- [ ] 🟡 Color-code byte roles (prefix / opcode / modrm / sib / disp / imm)
- [ ] 🟡 Total code size, instruction count

**Panel 7 — Benchmark & Tiering ⭐**
- [ ] 🔴 Bar chart: interpreter vs baseline vs optimized vs each SIMD width
- [ ] 🟡 Line chart: time vs input size, showing bandwidth saturation
- [ ] 🟡 **Tier-up timeline**: invocations on X, tier as a step function, compile spikes marked
- [ ] 🟡 Break-even callout: "compiling pays off after N calls"

**Panel 8 — Target Selector**
- [ ] 🟡 Toggle SSE2 / AVX2 / FMA / AVX-512 / NEON; toggle x86-64 / AArch64 / WASM
- [ ] 🟡 Panels 5–7 regenerate on change
- [ ] 🟡 **Side-by-side x86 vs AArch64** of the same function — 21 bytes of variable-length CISC vs 8 fixed 32-bit words

---

## Phase 16 — Docs & Polish (12 tasks)

- [ ] 🟢 `README.md` — what it is, the Rust rationale, quickstart, workbench link, headline benchmark
- [ ] 🟢 `docs/ENCODING.md` — x86-64 instruction format walkthrough with worked examples
- [ ] 🟢 `docs/REGALLOC.md` — linear scan explained with the interval diagrams
- [ ] 🟢 `docs/OPTIMIZATION.md` — every pass, every rule, and why FP validity differs
- [ ] 🟢 `docs/PLATFORMS.md` — W^X, MAP_JIT, icache, entitlements, Windows differences
- [ ] 🟡 Error messages naming the exact encoding constraint violated
- [ ] 🟡 `--verbose` tracing each compilation phase with timing
- [ ] 🟡 Benchmark table in the README with real machine numbers
- [ ] 🟡 `cargo clippy -- -D warnings`; `cargo fmt --check`; `bun run tsc --noEmit`
- [ ] 🔵 Loop support (`while`) with LICM and OSR
- [ ] 🔵 Instruction scheduling to shorten dependency chains
- [ ] 🔵 Profile-guided specialization: record observed value ranges, recompile with them as constants

---

## Summary

| Phase | Tasks |
|---|---|
| 0. Bootstrap | 14 |
| 1. Frontend | 20 |
| 2. SSA IR | 26 |
| 3. Interpreter (oracle) | 10 |
| 4. Optimizer | 32 |
| 5. Executable Memory | 18 |
| 6. x86-64 Encoder | 40 |
| 7. Instruction Selection & Prologue | 22 |
| 8. Register Allocation | 24 |
| 9. AArch64 Backend | 20 |
| 10. SIMD Vectorization | 22 |
| 11. Differential Testing | 18 |
| 12. Tiered Runtime | 12 |
| 13. CLI | 14 |
| 14. WASM Backend & Bindings | 14 |
| 15. Workbench | 34 |
| 16. Docs & Polish | 12 |
| **TOTAL** | **352** |
