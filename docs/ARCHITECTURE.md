# Architecture

Forge keeps the compiler pipeline explicit so each phase can be inspected and
tested independently.

```text
source
  │
  ├─ forge-syntax       lex, parse, resolve names, type-check, retain spans
  │
  ├─ forge-ir           lower to typed SSA, interpret, verify dominance/typing
  │
  ├─ forge-opt          fold, simplify, CSE, DCE, reassociate, reduce strength
  │
  ├─ forge-x64           select virtual machine instructions and encode x86-64
  ├─ forge-aarch64       encode and emit the supported AAPCS64 scalar subset
  ├─ forge-regalloc      build live intervals, allocate, spill, verify allocation
  ├─ forge-emit          lay out physical x86-64 instructions and constant pools
  └─ forge-mem           allocate W^X memory and execute finalized code

  ├─ forge-wasm           emit portable typed WebAssembly bytes
  ├─ forge-wasm-api       expose stable browser-facing JSON/wasm-bindgen APIs
  ├─ forge-simd           evaluate array bodies with feature-safe packed paths
  ├─ forge-runtime        select the backend, tier calls, and expose public APIs
  ├─ forge-cli             inspect and evaluate from a terminal
  └─ workbench             visualize compiler artifacts in a browser
```

## Semantic contract

The interpreter is the oracle. Optimizer passes are checked after each pass in
debug builds, and native differential tests compare result bits for the cases
where the host can execute generated code. Floating-point behavior deliberately
includes NaNs, infinities, signed zero, subnormals, and the platform libm
boundary. Integer operations use wrapping semantics.

## IR contract

`Function` stores instructions, types, source spans, blocks, predecessors, and
parameters in parallel tables. Each instruction value is listed in exactly one
block. The verifier checks table shape, CFG targets and predecessor lists,
same-block ordering, operand types, φ edge dominance, and ordinary dominance.
This makes hand-built or transformed IR fail with a diagnostic instead of
becoming an unchecked source of memory-unsafe code generation.

## Native contract

Native compilation is a sequence of selection, allocation, independent
allocation verification, layout, and emission. The x86-64 emitter owns the
calling-convention details for supported System V and Win64 shapes. The
AArch64 emitter follows AAPCS64 for supported scalar functions. Spills,
constant pools, branch labels, caller-saved values, and return registers are
resolved before executable memory is finalized.

## Portable contract

WASM is a stack-machine artifact and does not use native register allocation.
The browser-facing API exposes bytes, a decoded stack trace, stack depth,
logical lifetimes, IR, CFG, and diagnostics. Process-local native addresses are
never embedded in portable artifacts. The Workbench executes WASM and treats
native artifacts as read-only inspection data.

## Array contract

The canonical vectorized form uses indexed f64 columns and loop-invariant
broadcasts. Constant and typed dynamic offsets are checked against a shared
input window. Nested declarations use flattened row-major columns and checked
dimensions; arbitrary per-row gathers and arbitrary memory addressing remain
outside the language contract. Unsupported packed operations use the exact
scalar evaluator.
