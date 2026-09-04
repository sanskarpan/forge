# Platform behavior

## Executable memory

`forge-mem` enforces W^X. A buffer is allocated writable, populated through
the closure-based `write` API, and then sealed executable. Calls require an
executable buffer and are arity-checked at the single function-pointer
boundary. `Drop` unmaps the owned pages; `CodeCache` can recycle buffers only
after resetting their protection state.

On macOS AArch64, `MAP_JIT` pages use
`pthread_jit_write_protect_np(0/1)` and `sys_icache_invalidate`. The binary
must be codesigned with `com.apple.security.cs.allow-jit`; `entitlements.plist`
and the `make spike` target document the local setup. On generic Unix, the
implementation uses RW `mmap`, `mprotect(RX)`, and the platform cache-flush
path where required. Windows support is intentionally rejected at compile
time until its `VirtualAlloc`/`VirtualProtect` path can be tested.

## Execution targets

The native runtime executes emitted x86-64 code only on x86-64. ARM and other
hosts use the verified interpreter for portable evaluation, while the
selection/allocation/emission artifacts remain inspectable. This is why
`compile_artifacts` is separate from `compile`.

`make qemu-aarch64` uses a real cross target when Rust and QEMU are installed;
otherwise it runs the native AArch64 encoder tests and states the fallback.
The current AArch64 crate contains tested scalar encoding primitives, not yet
the full expression backend. The WASM crate emits dependency-free all-f64
scalar modules and has a portable interpreter facade; libm, integer/bool, and
wasm-bindgen surfaces remain explicit follow-up scope.

## Validation matrix

Run the portable checks with:

```sh
make test
cargo test --workspace --offline --locked
cargo clippy --workspace --offline --all-targets -- -D warnings
make qemu-aarch64 wasm workbench
```

The ARM development host can validate the interpreter, IR, optimizer,
allocator, encoder bytes, W^X mapping, WASM bytes, and workbench smoke test.
Only x86-64 execution tests require an x86-64 runner; this limitation is
target-gated and recorded rather than treated as passing native execution.
