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

The documented source-level array form is now available end to end:
`@vectorize result[i] = a[i] * b[i] + c[i]` parses indexed f64 columns,
while unindexed f64 names are scalar broadcasts, e.g.
`@vectorize result[i] = a[i] + scale`. Both forms lower to verified array
loop/memory IR and execute through the packed evaluator with scalar epilogues
and exact fallback. Supply broadcast values with
`forge_simd::evaluate_vectorized_with_broadcasts` (or its feature-masked
variant) separately from the input columns; packed backends splat them without
materializing repeated arrays. Indexed columns also support constant offsets,
for example `@vectorize result[i] = a[i + 1] - b[i - 2]`. The evaluator uses
the largest shared in-bounds window: negative offsets skip leading rows,
positive offsets trim trailing rows, and a window too short for the requested
offsets returns an empty result. The same column may be used at multiple
constant offsets, including stencil-style expressions such as
`a[i - 1] + a[i] + a[i + 1]`. Loop-invariant dynamic scalar offsets such as
`a[i + shift]` are supported through
`evaluate_vectorized_with_typed_broadcasts` with an `RtValue::I64`; the runtime
materializes the call-time offset into the same checked constant-offset window.
The f64-only broadcast APIs continue to reject these sources because their ABI
cannot carry an i64 value. Per-row array-valued indices and other genuinely
non-contiguous gathers remain outside the current one-dimensional language.
Nested source loops use declarations such as
`@vectorize result[i, j] = a[i * width + j] + bias`; callers pass flattened
row-major columns and dimensions to `evaluate_nested_vectorized`. That API
checks shape products, typed broadcasts, and every computed index before any
load, and uses the verified scalar evaluator for arbitrary nested addressing.
Array expressions containing
`sin`, `cos`, `tan`, `exp`, `log`, or `pow` retain packed execution through a
lane-preserving libm adapter that applies the interpreter's scalar operation to
each active lane and repacks the exact results. The full AArch64
expression backend remains an explicit scope boundary; pure acyclic
structured conditionals are already handled by the packed evaluator with lane
masks and predicated selects. WASM target artifacts now include a decoded
stack-machine instruction trace with byte offsets, stack depth, and logical
stack-value lifetimes; native register intervals remain target-specific. Array mode also uses packed floor/ceil/trunc instructions on AVX2,
AVX-512F, and NEON where their rounding semantics are exact; SSE4.1 width-2,
AVX2, and AVX-512F implement ties-away-from-zero `round` with exact packed
sequences, while SSE2-only and unsupported x86 widths use the scalar
interpreter fallback. Array
callers can use `evaluate_array_with_features`,
`evaluate_vectorized_with_features`,
`evaluate_vectorized_with_broadcasts_and_features`, or
`reduce_sum_with_features` to apply a host-safe SIMD feature mask;
`evaluate_vectorized` and `evaluate_vectorized_with_broadcasts` are the
convenience entries without an explicit mask. The scalar
mask forces the exact interpreter fallback. The tested wasm-bindgen artifact/benchmark API
and React workbench are available; the browser executes real WASM artifacts
and receives serialized x86-64/AArch64 inspection artifacts for supported
expressions. Native bytes are never executed in the browser, and expressions
requiring process-local libm addresses remain an explicit unavailable case.
These boundaries are explicit follow-up phases rather than hidden runtime
fallbacks; the current status table in `CHECKLIST.md` is authoritative.
