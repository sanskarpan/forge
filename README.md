# forge

`forge` is a small educational JIT compiler for typed mathematical
expressions. It has a hand-written lexer/parser, typed SSA IR, reference
interpreter, semantics-preserving optimizer, linear-scan register allocator,
W^X executable memory, and a hand-written x86-64 encoder/emitter.

The repository also contains portable scalar WASM and AArch64 encoder
foundations, runtime tiering, SIMD feature/array planning, a CLI, benchmarks,
and a dependency-free browser shell. The current implementation status and
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

The full AArch64 expression backend, packed SIMD loop/code generation,
wasm-bindgen artifact API, and React workbench are intentionally not claimed
as complete yet. They remain explicit follow-up phases rather than hidden
runtime fallbacks. The current status table in `CHECKLIST.md` is authoritative.
