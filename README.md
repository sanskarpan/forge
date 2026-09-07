# forge

`forge` is a small educational JIT compiler for typed mathematical
expressions. It has a hand-written lexer/parser, typed SSA IR, reference
interpreter, semantics-preserving optimizer, linear-scan register allocator,
W^X executable memory, and a hand-written x86-64 encoder/emitter.

The repository also contains portable scalar WASM and AArch64 encoder
foundations, runtime tiering, SIMD feature/array planning, a CLI, benchmarks,
and a React/Vite compiler workbench. The current implementation status and
remaining boundaries are tracked in [CHECKLIST.md](CHECKLIST.md); `SPEC.md` is
the design reference.

## Quick start

```sh
cargo test --workspace --offline --locked
cargo clippy --workspace --offline --all-targets -- -D warnings
cargo run -p forge-cli -- eval 'x * x + 1' --x 3
cargo run -p forge-cli -- asm 'x * x + 1'
cargo run -p forge-cli -- ir 'sqrt(x * x + y * y)'
cargo run -p forge-cli -- cfg 'if x > 0.0 then x else -x' --dot
npm ci --prefix workbench
npm test --prefix workbench
npm run build --prefix workbench
```

The native x86-64 JIT runs when built on x86-64. On other hosts, the runtime
uses the verified interpreter for evaluation while still exposing x86 bytes
and allocation artifacts for inspection. `make qemu-aarch64` runs a cross
test when QEMU and the target toolchain are installed, otherwise it runs the
native AArch64 encoder tests.

## Architecture

Source is lowered through `forge-syntax` and `forge-ir`, checked after each
optimizer pass, selected into virtual machine instructions, allocated into
physical registers/spill slots, and finally emitted into executable memory.
The interpreter is the correctness oracle; differential tests compare result
bits where the target can execute the JIT.

```text
source → syntax/types → SSA IR → optimize → select → allocate → emit → W^X JIT
                                      ↘ interpreter / WASM / inspection artifacts
```

The project writes its own x86 encodings. `iced-x86` is used only as a
disassembly test oracle. See [docs/ENCODING.md](docs/ENCODING.md),
[docs/REGALLOC.md](docs/REGALLOC.md), [docs/OPTIMIZATION.md](docs/OPTIMIZATION.md),
and [docs/PLATFORMS.md](docs/PLATFORMS.md) for the implementation details.

## Scope notes

The full AArch64 expression backend and general array-mode memory/loop IR
remain explicit scope boundaries; pure acyclic structured conditionals are
already handled by the packed evaluator with lane masks and predicated
selects. Array callers can use `evaluate_array_with_features` or
`reduce_sum_with_features` to apply a host-safe SIMD feature mask; the scalar
mask forces the exact interpreter fallback. The tested wasm-bindgen artifact/benchmark API
and React workbench are available; the browser executes real WASM artifacts
and receives serialized x86-64/AArch64 inspection artifacts for supported
expressions. Native bytes are never executed in the browser, and expressions
requiring process-local libm addresses remain an explicit unavailable case.
These boundaries are explicit follow-up phases rather than hidden runtime
fallbacks; the current status table in `CHECKLIST.md` is authoritative.
