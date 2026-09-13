# Checklist audit

Status date: 2026-09-14

This document is the implementation audit for [`CHECKLIST.md`](../CHECKLIST.md)
and [`SPEC.md`](../SPEC.md). The phase sections in `CHECKLIST.md` are retained
as historical design records: their unchecked markers describe the original
work plan and are not an inventory of missing code. Current release status is
determined from the source tree, tests, CI, and the scope decisions recorded
below.

## Verification performed

The following gates passed for the production-readiness integration and
promotion changes:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
npm ci --prefix workbench
npm test --prefix workbench
npm run typecheck --prefix workbench
npm run build --prefix workbench
./scripts/generate-demo-gif.sh /tmp/forge-pipeline-verification.gif
```

The repository CI matrix also passed on the promotion candidate:

- macOS build, workspace tests, Clippy, rustfmt, and codesign checks;
- Linux x86-64 container tests, Clippy, and rustfmt;
- Linux ARM64 QEMU container workspace tests;
- Windows native workspace tests and executable-memory coverage;
- WASM check, `wasm-pack`, `wasm-opt`, gzip-size, and Node execution;
- Workbench smoke, typecheck, production build, and dependency audit;
- mdBook build, CodeQL, RustSec, GitGuardian, and Linux Valgrind.

The ARM64 container job has a 30-minute timeout because its full workspace
suite includes the 100,000-expression differential corpus. This is a failure
boundary, not a reduction in correctness coverage.

## Current status by phase

| Phase | Release status | Resolution |
| --- | --- | --- |
| 0 Bootstrap | Complete | Workspace, dependency model, Make targets, platform matrix, and W^X foundations are present. |
| 1 Frontend | Complete | Lexer, parser, spans, diagnostics, AST, types, bindings, and property/round-trip tests are shipped. |
| 2 SSA IR | Complete | Builder, SSA construction, CFG, dominance, phi handling, textual IR, and defensive verification are shipped. |
| 3 Interpreter | Complete | The interpreter is the semantic oracle for all IR operations, IEEE behavior, wrapping integers, calls, and phi edges. |
| 4 Optimizer | Complete for safe optimization | Constant folding, validity-gated simplification, CSE/GVN, copy propagation, DCE, reassociation, FMA, and statistics are shipped. Magic-number division math is tested, but no unsafe IR rewrite is claimed without a widening multiply IR operation. |
| 5 Executable memory | Complete for supported hosts | Linux, macOS, AArch64 cache handling, Windows APIs, code cache, arity checks, W^X transitions, Valgrind, and native regressions are shipped. Miri remains inapplicable to raw executable-memory calls. |
| 6 x86-64 encoder | Complete for the supported instruction set | Scalar, control-flow, SSE, VEX, EVEX, ABI primitives, constant-pool addressing, disassembly, round trips, and golden bytes are covered. Unsupported encodings return explicit errors or are outside the supported target contract. |
| 7 Selection/emission | Complete for the supported scalar pipeline | Machine selection, LEA and safe diamond fusion, constant pools, ABI frames, calls, spills, phi copies, and native emission are integrated and tested. General maximal-munch tree tiling and arbitrary memory-address folding are not language requirements. |
| 8 Register allocation | Complete for shipped allocation paths | Liveness, inclusive intervals, hints, fixed-register handling, spilling, reload/store emission, independent verification, pressure reporting, and high-pressure execution are shipped. The verifier is intentionally narrow and is composed with other allocator contract checks. |
| 9 AArch64 | Complete for supported scalar ABI/codegen subset | Encoding, frames, scalar CFG, conversions, libm, typed parameter banks, stack arguments, spills, phi copies, external calls, native execution, and QEMU coverage are shipped. This is not a claim of complete AArch64 ISA coverage. |
| 10 SIMD | Complete for the documented vector boundary | Feature masking, widths, loads/stores, broadcasts, tails, masked AVX-512 tails, reductions, libm adapters, nested addressing, FMA, exact min/max/rounding, and scalar fallback are shipped. |
| 11 Differential verification | Complete for shipped targets | Fuzz safety, optimizer equivalence, cross-backend checks, IEEE edge cases, 100,000-expression native corpus, Valgrind, and CI matrix coverage are shipped. Miri cannot execute the raw JIT call path. |
| 12 Tiered runtime | Complete | Thread-safe interpreter, baseline, optimized promotion and stable tier behavior are shipped. |
| 13 CLI | Complete | The documented command surface, REPL, diagnostics, annotations, JSON benchmarks, target selection, and exit codes are shipped. |
| 14 WASM | Complete for the portable artifact contract | Typed stack-machine artifacts, real WebAssembly execution, fmod host import declaration, diagnostics, AST/IR/CFG metadata, benchmark API, and packaging are shipped. Native register intervals do not apply to WASM. |
| 15 Workbench | Complete for the browser-safe inspection contract | The Workbench builds and runs with the real WASM API, visualizes AST/IR/CFG/intervals/bytes/benchmarks, supports scalar/array and target selection, and never executes native bytes in the browser. |
| 16 Docs and polish | Complete for the production release surface | README, mdBook, architecture/testing/release/platform/encoding/optimization/register-allocation guides, governance files, security automation, Pages workflow, and demo GIF are shipped. |

## Additions verified in this audit

The current implementation also closes the previously missing observability
and reproducibility pieces that were actionable without changing the language
contract:

- `forge-cli --verbose` reports measured compilation-phase timings through a
  public runtime trace API, with an ordered runtime regression test.
- Native x86-64 artifact JSON includes GPR/XMM pressure samples computed from
  the allocator’s verified intervals; the Workbench renders those samples
  alongside native interval bars.
- The Workbench can encode the current source, arguments, target, and scalar or
  array mode in a share URL and restores that state on load.
- `scripts/generate-demo-gif.sh` captures the output of real CLI commands from
  the checkout. The checked-in GIF is generated from that script and is not a
  simulated compiler animation.

## Explicit scope boundaries

These items are intentionally resolved as release boundaries rather than
silent omissions:

1. **General tree tiling and arbitrary memory addressing.** Forge’s scalar IR
   has no general `Load`/`Store` pointer language. SIMD uses verified array
   memory IR with checked canonical and constant-offset addressing; arbitrary
   pointer arithmetic and maximal-munch addressing modes are not exposed.
2. **Full AArch64 ISA coverage.** The supported scalar and ABI subset is
   tested end to end. Instructions outside that subset must not be inferred
   to work from the encoder’s existence.
3. **Miri for executable memory.** Miri cannot model the raw executable
   mapping and function-pointer call. Valgrind and native platform lanes cover
   the supported executable-memory paths instead.
4. **Magic-number division rewriting.** The tested `magic_signed` math is
   retained, but the current IR only has a truncating 64-bit multiply. A
   correct high-half multiply rewrite requires a new widening IR operation or
   a code-generation-level lowering; the optimizer does not pretend otherwise.
5. **Fast-math-only transformations.** Floating-point reassociation and
   algebraic identities that alter NaN, infinity, signed-zero, or rounding
   behavior are not enabled in the bit-exact default pipeline. Platform-libm
   `pow` reductions that were not bit-exact are deliberately not emitted.
6. **OSR, loop LICM, instruction scheduling, and profile-guided specialization.**
   These are stretch roadmap items, not required behavior of the current
   expression and verified-array-loop contracts. No public API claims them.
7. **Native execution in the Workbench.** Native x86-64 and AArch64 output is
   inspection-only in the browser. WASM is the executable browser target.
8. **Benchmark targets.** The allocator benchmark is recorded with the real
   measured result (46.953 µs median and 49.740 µs upper estimate in the
   current validation run); a target is not marked achieved by changing the
   workload or weakening the assertion.

## Audit policy

Future feature work must update the current-status table in `CHECKLIST.md`,
add a regression at the narrowest proving layer, update the relevant SPEC
section, and add or revise an explicit boundary here when a checklist item is
scoped down. Historical checkboxes must not be mass-checked merely to make
the file appear complete.
