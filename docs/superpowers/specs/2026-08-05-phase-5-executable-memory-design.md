# Design: forge Phase 5 — Executable Memory

**Status:** Approved for planning
**Scope:** CHECKLIST.md Phase 5 — the real `ExecutableBuffer` abstraction (W^X, platform-specific write paths), `CompiledExpr`'s checked call API, a code cache, and the test suite proving W^X is actually enforced by the OS (not just "we didn't call the wrong syscall").
**Out of scope:** Windows (`VirtualAlloc`/`VirtualProtect`) — CHECKLIST marks it 🟡, and there's no way to test it from this machine or in CI yet. Left as an explicit, visible stub rather than shipped unverified. Real code generation (Phase 6/7) — tests here use hand-written machine-code bytes, the same pattern as the Phase 0 day-one spike, not a real compiler.

This slice replaces `forge-mem`'s placeholder `src/lib.rs` with the production W^X abstraction. The Phase 0 day-one spike (`crates/forge-mem/examples/spike.rs`) stays exactly as-is — a standalone, minimal proof that this machine's platform setup works — separate from and untouched by this real, reusable abstraction.

## The three-way platform split (the part most likely to get silently wrong)

It's tempting to treat this as "macOS ARM64 vs. everything else," matching the day-one spike's two-way `cfg`. That's wrong, and would be exactly the kind of "looks right, silently broken on a platform nobody tested" bug this project exists to avoid. There are two **independent** axes:

1. **`MAP_JIT` + `pthread_jit_write_protect_np`** — Apple Silicon's hardened-runtime requirement. macOS AArch64 only.
2. **Instruction-cache invalidation** — required because the icache isn't coherent with the dcache. This is an **AArch64 architecture property**, not an Apple one — Linux running on ARM64 hardware needs it exactly as much as macOS does. x86-64 (any OS) needs neither: no `MAP_JIT` requirement, and x86's icache is hardware-coherent with the dcache.

So the real split is three branches, not two:

| | `MAP_JIT` dance | icache invalidation |
|---|---|---|
| macOS + AArch64 | yes (`pthread_jit_write_protect_np`) | yes (`sys_icache_invalidate`) |
| other AArch64 (Linux AArch64) | no (plain `mmap`/`mprotect`) | yes (inline asm: `dc cvau` / `ic ivau` / `dsb ish` / `isb`) |
| x86-64 (any OS) | no | no |

Only the first row is empirically testable on this machine. The other two are written from documented syscall/instruction behavior, compile-checked via `cfg`, but **not run** here — this gets stated explicitly in the exit criteria and in code comments, not silently implied to be verified the way the macOS-ARM64 path is.

## `ExecutableBuffer`

```rust
pub enum ProtState { Writable, Executable }

pub struct ExecutableBuffer {
    ptr: *mut u8,
    len: usize,       // page-rounded via sysconf(_SC_PAGESIZE); a 0-byte request rounds up to one page, never zero-sized
    state: ProtState,
}

impl ExecutableBuffer {
    pub fn new(size: usize) -> io::Result<Self>;
    pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F);
    pub fn make_executable(&mut self) -> io::Result<()>;
    pub fn as_ptr(&self) -> *const u8;
    pub fn state(&self) -> ProtState;
}
```

`write()` is the **only** way to put bytes into the buffer — this makes the platform-specific protect/write/reprotect/invalidate sequence structurally impossible to get wrong or skip, the same design principle PROMPT.md's `ExecutableBuffer::write()` sketch already establishes.

The two platform families genuinely mean different things by "state," and the API has to paper over that difference for callers:
- **Generic Unix (x86-64 any OS, Linux AArch64):** `new()` maps `PROT_READ|WRITE` (deliberately never `PROT_EXEC` yet — never map RWX). `state` tracks the actual page permission bits. `make_executable()` is a real `mprotect(PROT_READ|EXEC)` call.
- **macOS AArch64:** `new()` maps `PROT_READ|WRITE|EXEC` with `MAP_JIT` — the page is always execute-capable from the OS's perspective; what's actually gated is per-*thread* write access via `pthread_jit_write_protect_np`. `write()` does its own protect-write-reprotect-invalidate dance internally on every call, regardless of `state`. `make_executable()` here is a state-only transition (no syscall) — it exists so caller code stays platform-agnostic (`buf.write(...); buf.make_executable()?; buf.call(...)`) even though the underlying mechanics differ.

**Concurrency:** `ExecutableBuffer` is `Send` (moving ownership to another thread and calling `write()`/executing there is safe — `pthread_jit_write_protect_np` is a per-thread hardware toggle, not tied to whichever thread created the mapping) but **not `Sync`** (concurrent `write()` calls from multiple threads on the same buffer need external synchronization the type doesn't provide).

## `CompiledExpr`

```rust
pub struct CompiledExpr { buf: ExecutableBuffer, arity: usize }

impl CompiledExpr {
    pub fn call1(&self, x: f64) -> f64;
    pub fn call2(&self, x: f64, y: f64) -> f64;
    pub fn call_n(&self, args: &[f64]) -> f64;   // unsafe extern "C" fn(*const f64) -> f64
}
```

Each checks `arity` with a real `assert!` (not `debug_assert!` — calling with the wrong arity is a caller bug that must fail loudly in release builds too, not just in debug), then does the single documented `transmute` to a function pointer and calls it. This remains the one and only place in the codebase that performs this transmute, per the project's own stated invariant.

**Correction found during implementation (Task 3):** the `state() == Executable` check was originally written as a `debug_assert!`, matching this doc's original wording. It was promoted to a real `assert!` (in `CompiledExpr::from_buffer`, and again in each of `call1`/`call2`/`call_n`) because on macOS AArch64 — the only platform this crate is actually tested on — `ExecutableBuffer::new()` maps `PROT_READ|WRITE|EXEC` unconditionally via `MAP_JIT`, and `make_executable()` there is a pure state-field flip with no syscall backing it. So on that platform this assert is the *entire* protection against transmuting-and-calling into an unfinished or partially-written buffer — there is no OS-level backstop (e.g. a SIGSEGV from calling into a non-executable page) to fall back on the way there is on the generic-Unix path, which is why it can't be release-build-optional there.

Since there's no code generator yet, tests build `CompiledExpr`s from small hand-written byte sequences (arch-appropriate identity/arithmetic functions), the same pattern the day-one spike already established — not real compiled output.

## Code cache

A minimal free-list, not an elaborate size-class allocator — YAGNI given no real compiler pipeline exists yet to stress it:

```rust
pub struct CodeCache { free: Vec<ExecutableBuffer> }

impl CodeCache {
    pub fn acquire(&mut self, min_size: usize) -> io::Result<ExecutableBuffer>;
    pub fn release(&mut self, buf: ExecutableBuffer);
}
```

`acquire` reuses a large-enough buffer from the free-list if one exists, else allocates fresh via `ExecutableBuffer::new`. `release` resets the buffer back to a writable state for reuse — a real `mprotect(PROT_READ|WRITE)` on the generic-Unix path, a state-field reset only on macOS AArch64 (since `write()` there always does its own per-call protect dance regardless of the buffer's nominal state, there's nothing to actually "unprotect" up front).

## Testing plan

- **Basic execute test**: allocate → `write()` the arch-appropriate identity-function bytes (reusing the day-one spike's `uname -m` detection pattern) → `make_executable()` → call through `CompiledExpr::call1` → confirm the answer. The direct successor to the spike, now through the real, reusable abstraction instead of one-off raw syscalls in a `main()`.
- **W^X enforcement test** ("the buffer is not writable after `make_executable`"): cannot be tested by attempting an illegal write from the test process directly — that's UB, and would either crash or silently corrupt the whole test binary depending on what happens to be at that address. The correct technique: `fork()` a child process, have the child attempt the illegal write, assert (in the parent, via `waitpid`) that the child died from `SIGSEGV` (`WIFSIGNALED` + `WTERMSIG == SIGSEGV`). This is itself unsafe, `fork`-based test code needing its own `SAFETY` comments — but it's the only way to prove the OS is *actually* enforcing W^X, rather than merely "we didn't call the wrong protection flags and got lucky."
- **Leak test**: `getrusage(RUSAGE_SELF).ru_maxrss` is a high-water mark, not a live RSS reading, so it can't be expected to decrease after freeing buffers. The test instead compares the high-water mark's *growth rate* across 10,000 alloc/free (via `Drop`) cycles — flat growth after an initial warmup means the `munmap`-on-`Drop` path works; growth that scales linearly with iteration count means a real leak.
- **Miri**: explicitly scoped **out** of automated CI for this crate. Miri cannot model raw `mmap`/`mprotect` syscalls or a transmute-to-function-pointer call under isolation — CHECKLIST's own "test under Miri where possible" hedge resolves to "not meaningfully possible here," stated as a documented non-goal rather than a silently-skipped checklist line.

## Exit criteria

1. Basic execute test passes on this machine (macOS ARM64).
2. W^X-enforcement fork/`SIGSEGV` test passes on this machine.
3. Leak test shows flat high-water-mark growth over 10,000 alloc/free cycles.
4. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
5. The x86-64 and Linux-AArch64 code paths compile cleanly under their respective `cfg` targets (checked via `cargo check --target ...` where a target is installed, or at minimum via careful code review against documented syscall behavior where cross-compilation isn't available) — explicitly documented in code comments as compile-checked-only, not run.
6. Windows is a visible, explicit stub (a `compile_error!` or a clearly-named unimplemented module) — not silently absent, not attempted without a way to verify it.
