# forge Phase 5 Executable Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `forge-mem`'s placeholder with the real W^X `ExecutableBuffer` abstraction, `CompiledExpr`'s checked call API, and a minimal code cache — with tests that prove W^X is genuinely enforced by the OS (fork/`SIGSEGV`) and that 10,000 allocate/free cycles don't leak.

**Architecture:** A `write()`-only-entry-point `ExecutableBuffer` with a real three-way platform split (macOS+AArch64 needs `MAP_JIT`; any AArch64 needs icache invalidation; x86-64 needs neither) — only the macOS-ARM64 path is empirically testable on this machine, the rest are compile-checked via cross-target `cargo check`. `CompiledExpr` wraps a buffer with an arity-checked call API, tested against small hand-written machine-code bytes (no compiler exists yet).

**Tech Stack:** Rust, `libc` for raw syscalls, no new crate dependencies (matching the project's "we do this by hand" philosophy already established for the day-one spike).

**Design doc:** `docs/superpowers/specs/2026-08-05-phase-5-executable-memory-design.md` — read this first, especially the "three-way platform split" section, before implementing.

---

## Task 1: Core types + macOS AArch64 platform impl (the only empirically-testable path)

**Files:**
- Modify: `crates/forge-mem/src/lib.rs` (overwrites the Task-1-era placeholder from Phase 0)

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-mem/src/lib.rs — append at the bottom

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_write_execute_roundtrip() {
        let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");
        assert_eq!(buf.state(), ProtState::Writable);

        // AArch64 identity function: a bare `ret` IS the identity, since
        // AAPCS64 puts the first integer argument AND the return value in
        // the same register (x0). Same payload as the Phase 0 day-one
        // spike, already proven correct on this exact machine.
        buf.write(|mem| {
            mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]);
        });

        buf.make_executable().expect("make_executable should succeed");
        assert_eq!(buf.state(), ProtState::Executable);

        // SAFETY: the buffer holds a complete, valid `ret`-only function
        // body matching AAPCS64's fn(i64) -> i64 calling convention (x0 in,
        // x0 out); the page is executable and alive for this call.
        let f: unsafe extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        let result = unsafe { f(42) };
        assert_eq!(result, 42);
    }

    #[test]
    fn a_zero_size_request_rounds_up_to_one_page() {
        let buf = ExecutableBuffer::new(0).expect("should not fail on a 0-byte request");
        assert!(buf.len() > 0);
    }

    #[test]
    fn size_is_rounded_up_to_a_page_multiple() {
        let page = page_size();
        let buf = ExecutableBuffer::new(1).expect("allocation should succeed");
        assert_eq!(buf.len() % page, 0);
        assert!(buf.len() >= page);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-mem --lib 2>&1 | head -20`
Expected: FAIL — `ExecutableBuffer`/`ProtState`/`page_size` not defined.

- [ ] **Step 3: Write the implementation above the test module**

```rust
// crates/forge-mem/src/lib.rs — above the `#[cfg(test)]` module

use std::io;

/// Whether an `ExecutableBuffer`'s memory is currently writable or
/// executable — never both at once (W^X). See the design doc's "three-way
/// platform split" section for why what this actually MEANS differs
/// between platforms even though the API is uniform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProtState {
    Writable,
    Executable,
}

/// A page of memory that starts writable, gets machine code written into
/// it via `write()` (the ONLY way to write — this makes the
/// protect/write/reprotect/invalidate sequence structurally impossible to
/// skip), then gets flipped to executable via `make_executable()`. Never
/// mapped read+write+exec simultaneously.
pub struct ExecutableBuffer {
    ptr: *mut u8,
    len: usize,
    state: ProtState,
}

// SAFETY: `ExecutableBuffer` owns its mapping exclusively. Moving it to
// another thread and calling `write()`/executing there is safe --
// `pthread_jit_write_protect_np` (macOS AArch64) is a per-thread hardware
// toggle, not tied to whichever thread created the mapping, and the
// generic-Unix `mprotect`-based path has no thread affinity at all.
unsafe impl Send for ExecutableBuffer {}
// Deliberately NOT Sync: concurrent write() calls on the same buffer from
// multiple threads need external synchronization this type doesn't provide.

pub fn page_size() -> usize {
    // SAFETY: sysconf(_SC_PAGESIZE) has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

fn round_up_to_page(size: usize, page: usize) -> usize {
    let size = size.max(1); // never request a zero-sized mapping
    (size + page - 1) & !(page - 1)
}

impl ExecutableBuffer {
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn state(&self) -> ProtState {
        self.state
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod platform {
    use super::*;
    use std::ptr;

    extern "C" {
        fn pthread_jit_write_protect_np(enabled: libc::c_int);
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }

    impl ExecutableBuffer {
        pub fn new(size: usize) -> io::Result<Self> {
            let page = page_size();
            let len = round_up_to_page(size, page);
            // SAFETY: null hint, non-zero page-multiple length, valid flag
            // combination. MAP_JIT requires the com.apple.security.cs.allow-jit
            // entitlement on a signed binary -- we check the return value
            // and report a clear error rather than dereferencing MAP_FAILED.
            let ptr = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "mmap MAP_JIT failed: {} -- is com.apple.security.cs.allow-jit present \
                         in the entitlements, and is the binary codesigned?",
                        io::Error::last_os_error()
                    ),
                ));
            }
            Ok(Self { ptr: ptr as *mut u8, len, state: ProtState::Writable })
        }

        pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F) {
            // SAFETY: ptr/len come from a successful MAP_JIT mmap in new().
            // pthread_jit_write_protect_np(0) grants THIS thread write
            // access to MAP_JIT pages (mprotect cannot be used on them --
            // it returns EACCES); f only ever sees a correctly-bounded
            // slice; write access is revoked and the icache invalidated
            // before this function returns, so no other thread can
            // observe a state where the page is both writable and
            // stale-icache at once.
            unsafe {
                pthread_jit_write_protect_np(0);
                let slice = std::slice::from_raw_parts_mut(self.ptr, self.len);
                f(slice);
                pthread_jit_write_protect_np(1);
                sys_icache_invalidate(self.ptr as *mut libc::c_void, self.len);
            }
        }

        pub fn make_executable(&mut self) -> io::Result<()> {
            // No syscall needed here: MAP_JIT pages are already
            // execute-capable from mmap() in new(), and write() above
            // already re-protects + invalidates the icache after every
            // call. This method exists only so caller code stays
            // platform-agnostic -- see the generic-Unix impl in Task 2,
            // where this DOES perform a real mprotect.
            self.state = ProtState::Executable;
            Ok(())
        }
    }

    impl Drop for ExecutableBuffer {
        fn drop(&mut self) {
            // SAFETY: ptr/len are from a successful mmap in new(); munmap
            // on a mapping we exclusively own and are the only owner of.
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-mem --lib 2>&1 | tail -20`
Expected: 3 tests pass.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-mem/src/lib.rs
git commit -m "feat(forge-mem): ExecutableBuffer core types + macOS AArch64 W^X path"
```

## Context for this task

`forge-mem` currently has a one-line placeholder `src/lib.rs` (from Phase 0's workspace scaffold) and the standalone `examples/spike.rs` day-one spike (untouched by this task — it stays as its own separate proof). This task builds the REAL, reusable abstraction. Only this platform's (macOS AArch64) code path is written and tested in this task; Task 2 adds the generic-Unix path (x86-64 + Linux AArch64), which won't even be compiled by a normal `cargo build` on this machine since the `cfg` gate excludes it entirely here.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 2: Generic-Unix platform impl (x86-64 + Linux AArch64) — compile-checked only, not runtime-tested

**Files:**
- Modify: `crates/forge-mem/src/lib.rs`

**This code cannot be exercised on this machine** (macOS ARM64) — the `cfg` gate below excludes it entirely from a normal build here. Verification for this task means CROSS-COMPILE CHECKING it, not running tests.

- [ ] **Step 1: Install cross-compilation targets**

Run: `rustup target add x86_64-apple-darwin` (an x86-64 target buildable from this Mac without needing a separate cross-linker toolchain) and `rustup target add aarch64-unknown-linux-gnu` (to type-check the Linux-AArch64 icache-clear inline asm specifically).

- [ ] **Step 2: Write the implementation**

```rust
// crates/forge-mem/src/lib.rs — new module, alongside the macOS-AArch64 `platform` module from Task 1

#[cfg(all(unix, not(all(target_os = "macos", target_arch = "aarch64"))))]
mod platform {
    use super::*;
    use std::ptr;

    impl ExecutableBuffer {
        pub fn new(size: usize) -> io::Result<Self> {
            let page = page_size();
            let len = round_up_to_page(size, page);
            // SAFETY: null hint, non-zero page-multiple length, valid flag
            // combination. Deliberately PROT_READ|WRITE only -- NEVER map
            // RWX. Returns MAP_FAILED on error, which we check.
            let ptr = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { ptr: ptr as *mut u8, len, state: ProtState::Writable })
        }

        pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F) {
            debug_assert_eq!(
                self.state,
                ProtState::Writable,
                "cannot write after make_executable on this platform -- call a fresh buffer or add a make_writable() if this becomes a real need"
            );
            // SAFETY: ptr/len are from a successful RW mmap in new(); f
            // only ever sees a correctly-bounded slice.
            unsafe {
                let slice = std::slice::from_raw_parts_mut(self.ptr, self.len);
                f(slice);
            }
            clear_icache_if_needed(self.ptr, self.len);
        }

        pub fn make_executable(&mut self) -> io::Result<()> {
            // SAFETY: ptr/len are page-aligned, from a successful mmap.
            let rc = unsafe {
                libc::mprotect(self.ptr as *mut libc::c_void, self.len, libc::PROT_READ | libc::PROT_EXEC)
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            self.state = ProtState::Executable;
            Ok(())
        }
    }

    impl Drop for ExecutableBuffer {
        fn drop(&mut self) {
            // SAFETY: ptr/len are from a successful mmap in new(); munmap
            // on a mapping we exclusively own and are the only owner of.
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn clear_icache_if_needed(ptr: *mut u8, len: usize) {
        // Linux AArch64: the icache is not coherent with the dcache -- same
        // ARCHITECTURAL reason as macOS AArch64 (this is an ARM property,
        // not an Apple one), so it needs the same treatment even though
        // there's no MAP_JIT/pthread_jit_write_protect_np dance here.
        // Clean each cache line to the point of unification, invalidate the
        // icache, with the required barriers between each phase.
        //
        // UNTESTED on real hardware (no Linux ARM64 machine available for
        // this project) -- written from documented ARM64 cache-maintenance
        // instructions (DC CVAU / IC IVAU / DSB / ISB) and cross-compile
        // checked only. The 64-byte stride is a conservative assumption
        // (common ARM64 L1 cache line size); a smaller-than-actual stride
        // is always safe (just redundant work), so this errs safe even if
        // the real hardware's line size differs.
        //
        // SAFETY: ptr/len describe a valid, currently-mapped region this
        // ExecutableBuffer exclusively owns; the asm blocks only read
        // `addr` and issue architecturally-defined cache-maintenance
        // instructions -- no memory is written by this function.
        unsafe {
            let start = ptr as usize;
            let end = start + len;

            let mut addr = start & !63;
            while addr < end {
                std::arch::asm!("dc cvau, {0}", in(reg) addr);
                addr += 64;
            }
            std::arch::asm!("dsb ish");

            let mut addr = start & !63;
            while addr < end {
                std::arch::asm!("ic ivau, {0}", in(reg) addr);
                addr += 64;
            }
            std::arch::asm!("dsb ish");
            std::arch::asm!("isb");
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn clear_icache_if_needed(_ptr: *mut u8, _len: usize) {
        // x86-64: the icache is hardware-coherent with the dcache. Nothing
        // to do -- this empty fn exists so `write()`'s call site doesn't
        // need its own `#[cfg]` split.
    }
}
```

- [ ] **Step 3: Cross-compile-check both targets**

Run: `cargo check -p forge-mem --target x86_64-apple-darwin` and `cargo check -p forge-mem --target aarch64-unknown-linux-gnu`
Expected: both succeed with no errors. If either fails, fix the code (not the check) — this is the only verification this platform code gets, so it needs to actually pass, not be worked around.

- [ ] **Step 4: Confirm the normal build on THIS machine still only picks the macOS-AArch64 path**

Run: `cargo check -p forge-mem` (no `--target`, i.e. the host target)
Expected: succeeds, and this module's code is entirely excluded by `cfg` (confirm by temporarily adding a deliberate syntax error inside this module and confirming a normal `cargo check -p forge-mem` on this machine does NOT fail — then remove the deliberate error). This proves the two `platform` modules are mutually exclusive per-target, not both silently compiled (which would be a duplicate-definition error) or both silently skipped (which would mean neither is really gated correctly).

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found (run these on the HOST target — clippy/fmt don't need to run cross-target since Step 3 already validated compilation)**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-mem/src/lib.rs
git commit -m "feat(forge-mem): generic-Unix W^X path (x86-64, Linux AArch64) — cross-compile-checked, not runtime-tested"
```

## Context for this task

Be explicit in your final report about exactly what WAS and WASN'T verified — "compiles under `cargo check --target x86_64-apple-darwin` and `--target aarch64-unknown-linux-gnu`" is a true, useful claim; "works on Linux" would be false. This task's whole point is writing platform code honestly labeled as unverified where it genuinely is, per the design doc's stated philosophy.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`

---

## Task 3: `CompiledExpr` — arity-checked call API

**Files:**
- Modify: `crates/forge-mem/src/lib.rs`

**Before writing the hand-derived byte sequences below into a test, independently verify them.** A reasonable method: write the equivalent tiny function in a scratch Rust file (`#[no_mangle] pub extern "C" fn f(x: f64, y: f64) -> f64 { x + y }`), compile it (`rustc --crate-type=lib -O scratch.rs -o /tmp/scratch.o` or via a throwaway example in this crate), and disassemble the result with `otool -tv /tmp/scratch.o` (ships with Xcode Command Line Tools on macOS) to confirm the actual instruction encoding matches what's given below before trusting it. This project's whole ethos is "verify empirically, don't guess" — treat hand-derived machine code exactly the same way the day-one spike's bytes were treated: confirmed against real behavior, not assumed correct because they look right.

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-mem/src/lib.rs — append to the `#[cfg(test)] mod tests` block from Task 1

#[test]
fn call1_identity() {
    let mut buf = ExecutableBuffer::new(64).unwrap();
    // AAPCS64: fn(f64) -> f64 identity is a bare `ret` -- the argument and
    // return value are both in d0, same register, so nothing needs moving.
    // (Same reasoning as the i64 case in Task 1's test, just a different
    // register file.) VERIFY this independently before trusting it (see
    // this task's header note) -- it's a strong claim resting on AAPCS64's
    // calling convention, not something to eyeball.
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable().unwrap();
    let compiled = CompiledExpr::from_buffer(buf, 1);
    assert_eq!(compiled.call1(3.5), 3.5);
}

#[test]
fn call2_add() {
    let mut buf = ExecutableBuffer::new(64).unwrap();
    // fn(f64, f64) -> f64 { x + y }: `fadd d0, d0, d1; ret`.
    // VERIFY this byte sequence independently (see this task's header
    // note) before trusting it -- don't just transcribe it.
    buf.write(|mem| {
        mem[..8].copy_from_slice(&[
            0x00, 0x28, 0x61, 0x1E, // fadd d0, d0, d1
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ]);
    });
    buf.make_executable().unwrap();
    let compiled = CompiledExpr::from_buffer(buf, 2);
    assert_eq!(compiled.call2(2.0, 3.0), 5.0);
}

#[test]
fn call_n_reads_first_element() {
    let mut buf = ExecutableBuffer::new(64).unwrap();
    // fn(*const f64) -> f64 { *ptr }: the pointer arrives in x0 (integer
    // register, since it's a pointer not a float); load it into d0 and
    // return. `ldr d0, [x0]; ret`. VERIFY this byte sequence independently
    // before trusting it.
    buf.write(|mem| {
        mem[..8].copy_from_slice(&[
            0x00, 0x00, 0x40, 0xFD, // ldr d0, [x0]
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ]);
    });
    buf.make_executable().unwrap();
    let compiled = CompiledExpr::from_buffer(buf, 1);
    assert_eq!(compiled.call_n(&[9.5, 100.0]), 9.5);
}

#[test]
#[should_panic(expected = "arity mismatch")]
fn call1_panics_on_arity_mismatch() {
    let mut buf = ExecutableBuffer::new(64).unwrap();
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable().unwrap();
    let compiled = CompiledExpr::from_buffer(buf, 2); // arity 2, but we call call1
    compiled.call1(1.0);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-mem --lib call1 call2 call_n 2>&1 | head -20`

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-mem/src/lib.rs — above the `#[cfg(test)]` module, after ExecutableBuffer's impl block

/// A page of executable memory, arity-checked at the call boundary. This is
/// the single most dangerous operation in the crate, isolated to these
/// three methods -- the ONLY places `transmute` to a function pointer
/// happens anywhere in this codebase.
pub struct CompiledExpr {
    buf: ExecutableBuffer,
    arity: usize,
}

impl CompiledExpr {
    pub fn from_buffer(buf: ExecutableBuffer, arity: usize) -> Self {
        Self { buf, arity }
    }

    pub fn call1(&self, x: f64) -> f64 {
        assert_eq!(self.arity, 1, "arity mismatch: compiled for {} argument(s), called via call1", self.arity);
        debug_assert_eq!(self.buf.state(), ProtState::Executable);
        // SAFETY: arity checked above; state checked above in debug builds;
        // the buffer contains a complete function honoring AAPCS64/SysV's
        // fn(f64) -> f64 convention by construction -- this phase's own
        // tests are the only thing writing bytes into any buffer this type
        // wraps (no compiler exists yet to violate that contract).
        let f: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x) }
    }

    pub fn call2(&self, x: f64, y: f64) -> f64 {
        assert_eq!(self.arity, 2, "arity mismatch: compiled for {} argument(s), called via call2", self.arity);
        debug_assert_eq!(self.buf.state(), ProtState::Executable);
        // SAFETY: same as call1, for fn(f64, f64) -> f64.
        let f: unsafe extern "C" fn(f64, f64) -> f64 = unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x, y) }
    }

    pub fn call_n(&self, args: &[f64]) -> f64 {
        assert_eq!(self.arity, args.len(), "arity mismatch: compiled for {} argument(s), called via call_n with {}", self.arity, args.len());
        debug_assert_eq!(self.buf.state(), ProtState::Executable);
        // SAFETY: same as call1, for fn(*const f64) -> f64; `args` outlives
        // this call and its pointer is valid for `args.len()` reads, which
        // is what the arity check above guarantees the callee expects.
        let f: unsafe extern "C" fn(*const f64) -> f64 = unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(args.as_ptr()) }
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-mem --lib 2>&1 | tail -30`
Expected: all pass, including the 4 new tests. If `call2_add` or `call_n_reads_first_element` fail, the byte sequences are wrong — re-derive them via the independent-verification method from this task's header, don't just try random byte tweaks.

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-mem/src/lib.rs
git commit -m "feat(forge-mem): CompiledExpr arity-checked call API"
```

---

## Task 4: Code cache

**Files:**
- Modify: `crates/forge-mem/src/lib.rs`

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-mem/src/lib.rs — append to the `#[cfg(test)] mod tests` block

#[test]
fn code_cache_reuses_a_released_buffer() {
    let mut cache = CodeCache::default();
    let buf1 = cache.acquire(64).unwrap();
    let ptr1 = buf1.as_ptr();
    cache.release(buf1);

    let buf2 = cache.acquire(64).unwrap();
    assert_eq!(buf2.as_ptr(), ptr1, "acquire() should reuse the released buffer's mapping, not allocate a fresh one");
}

#[test]
fn code_cache_allocates_fresh_when_nothing_reusable_is_large_enough() {
    let mut cache = CodeCache::default();
    let small = cache.acquire(64).unwrap();
    let small_ptr = small.as_ptr();
    cache.release(small);

    let big = cache.acquire(page_size() * 4).unwrap();
    assert_ne!(big.as_ptr(), small_ptr, "a too-small released buffer must not be reused for a bigger request");
}

#[test]
fn released_buffer_is_writable_again() {
    let mut cache = CodeCache::default();
    let mut buf = cache.acquire(64).unwrap();
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable().unwrap();
    cache.release(buf);

    let mut reused = cache.acquire(64).unwrap();
    assert_eq!(reused.state(), ProtState::Writable, "a reused buffer must come back in a writable state, ready for a fresh write()");
    // Confirm it's genuinely usable, not just claiming to be writable:
    reused.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    reused.make_executable().unwrap();
}
```

- [ ] **Step 2: Run to confirm failure**

- [ ] **Step 3: Write the implementation**

```rust
// crates/forge-mem/src/lib.rs — above the `#[cfg(test)]` module

/// A minimal free-list, not a size-class allocator -- there's no real
/// compiler pipeline yet to stress this with varied allocation patterns.
/// Reuses `mmap`'d pages across compilations instead of syscalling on every
/// single one.
#[derive(Default)]
pub struct CodeCache {
    free: Vec<ExecutableBuffer>,
}

impl CodeCache {
    pub fn acquire(&mut self, min_size: usize) -> io::Result<ExecutableBuffer> {
        if let Some(pos) = self.free.iter().position(|b| b.len() >= min_size) {
            return Ok(self.free.remove(pos));
        }
        ExecutableBuffer::new(min_size)
    }

    pub fn release(&mut self, mut buf: ExecutableBuffer) {
        buf.reset_to_writable().expect("failed to reset a previously-valid ExecutableBuffer back to writable during release");
        self.free.push(buf);
    }
}
```

This calls a new `reset_to_writable()` method that doesn't exist yet — add it to `ExecutableBuffer`'s platform-specific impls (both `platform` modules from Tasks 1 and 2), with a UNIFORM `io::Result<()>` signature on both platforms even though only one of them can actually fail — a uniform signature is much easier for `CodeCache::release` (and any future caller) to reason about than a `#[cfg]`-gated difference. `release()` itself stays infallible (`Vec::push`-like ergonomics for callers) and `.expect()`s on failure: a working buffer suddenly failing to be re-protected during release is a genuinely exceptional, not-really-recoverable condition (something is deeply wrong with the process's memory state), not a normal error path to propagate.

In Task 1's macOS-AArch64 `platform` module, add:
```rust
impl ExecutableBuffer {
    /// Resets the buffer's nominal state to `Writable` for reuse by
    /// `CodeCache`. No syscall needed on this platform -- `write()` always
    /// does its own per-call protect dance regardless of the buffer's
    /// nominal state, so there's nothing to actually "unprotect" up front.
    /// Always succeeds; returns `io::Result<()>` only to keep a uniform
    /// signature with the generic-Unix platform module, where this CAN fail.
    pub fn reset_to_writable(&mut self) -> io::Result<()> {
        self.state = ProtState::Writable;
        Ok(())
    }
}
```

In Task 2's generic-Unix `platform` module, add:
```rust
impl ExecutableBuffer {
    /// Resets the buffer back to a real RW mapping for reuse by
    /// `CodeCache` -- unlike the macOS-AArch64 path, this platform's
    /// `write()` requires the page to genuinely be in `Writable` state
    /// already (see its `debug_assert_eq!`), so this needs a real
    /// `mprotect` call, not just a state-field reset.
    pub fn reset_to_writable(&mut self) -> io::Result<()> {
        // SAFETY: ptr/len are page-aligned, from a successful mmap.
        let rc = unsafe {
            libc::mprotect(self.ptr as *mut libc::c_void, self.len, libc::PROT_READ | libc::PROT_WRITE)
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        self.state = ProtState::Writable;
        Ok(())
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-mem --lib 2>&1 | tail -30`

- [ ] **Step 5: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-mem/src/lib.rs
git commit -m "feat(forge-mem): code cache with a free-list"
```

---

## Task 5: W^X enforcement test (fork/`SIGSEGV`)

**Files:**
- Create: `crates/forge-mem/tests/wx_enforcement.rs`

This is `fork()`-based test code, itself genuinely unsafe and needing its own `SAFETY` comments — the ONLY way to prove the OS is actually enforcing W^X (an illegal write from the SAME process would be UB and could corrupt or crash the whole test binary in an unpredictable way, not just the one assertion).

- [ ] **Step 1: Write the test**

```rust
// crates/forge-mem/tests/wx_enforcement.rs

use forge_mem::ExecutableBuffer;

/// Proves W^X is genuinely enforced by the OS, not just "we didn't call
/// mprotect(W) after make_executable and got lucky." Forks a child process,
/// has the child attempt an illegal write to an executable page, and
/// confirms (in the parent, via waitpid) that the child died from SIGSEGV.
/// Running the illegal write in a forked child (not the test process
/// itself) means a genuine W^X violation only kills the disposable child,
/// not the whole test binary.
#[test]
fn writing_to_an_executable_page_segfaults() {
    let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable().expect("make_executable should succeed");
    let ptr = buf.as_ptr() as *mut u8;

    // SAFETY: fork() has no preconditions beyond "don't do this in a
    // multi-threaded process without knowing what you're doing" -- this
    // test binary's threading is entirely under our control here, and
    // only async-signal-safe operations happen in the child before it
    // either segfaults or exits, avoiding the classic post-fork deadlock
    // hazards (no allocation, no locking, no println! in the child).
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());

    if pid == 0 {
        // Child: attempt the illegal write. If W^X is genuinely enforced,
        // this line never returns -- the process receives SIGSEGV here.
        // SAFETY: deliberately violating memory safety to prove the OS
        // stops us -- `ptr` points at a real page from the parent's
        // mapping (fork() gives the child the same virtual address space
        // via copy-on-write), which is executable-not-writable; this is
        // the entire point of the test.
        unsafe {
            std::ptr::write_volatile(ptr, 0xFFu8);
        }
        // If we reach here, W^X was NOT enforced -- exit with a distinct,
        // non-zero code so the parent can tell "wrote successfully" apart
        // from "was killed by a signal."
        std::process::exit(123);
    }

    // Parent: wait for the child and confirm it died from SIGSEGV.
    let mut status: libc::c_int = 0;
    // SAFETY: pid is the value fork() just returned to us in the parent
    // branch (guaranteed > 0 here since we're not in the pid==0 branch);
    // `status` is a valid, exclusively-owned local we're passing by
    // pointer for waitpid to write into.
    let wait_result = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(wait_result, pid, "waitpid failed: {}", std::io::Error::last_os_error());

    let signaled = libc::WIFSIGNALED(status);
    assert!(
        signaled,
        "expected the child to be killed by a signal (W^X enforced), but it exited normally with status {status}"
    );
    let sig = libc::WTERMSIG(status);
    // NOTE: an earlier fork-based regression test in this crate
    // (crates/forge-mem/tests/write_panic_protection.rs, from Task 1)
    // empirically found that a MAP_JIT write-protect violation on this
    // machine raises SIGBUS (10), not SIGSEGV (11) -- both are valid
    // hardware-fault signals for a protection violation, and which one a
    // given OS/kernel chooses is not part of the portable contract we're
    // testing here (we only care THAT the OS enforces W^X, not which
    // signal it happens to use to do so). Accept either rather than
    // asserting SIGSEGV specifically; if this test fails with a different
    // signal on some future platform, that's worth investigating, but
    // SIGBUS vs SIGSEGV specifically is already confirmed to vary.
    assert!(
        sig == libc::SIGSEGV || sig == libc::SIGBUS,
        "expected SIGSEGV or SIGBUS (W^X enforcement fault), got signal {sig}"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p forge-mem --test wx_enforcement 2>&1 | tail -20`
Expected: 1 test passes. If it fails because the child exited with code 123 (the write succeeded), that means W^X is NOT actually being enforced on this machine/build — this would be a serious finding worth investigating (is the buffer actually executable-not-writable? did `make_executable`/the macOS-AArch64 `write()` dance genuinely run?) rather than something to work around.

- [ ] **Step 3: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 4: Commit**

```bash
git add crates/forge-mem/tests/wx_enforcement.rs
git commit -m "test(forge-mem): W^X is genuinely enforced by the OS (fork/SIGSEGV)"
```

---

## Task 6: Leak test (10,000 allocate/free cycles)

**Files:**
- Create: `crates/forge-mem/tests/no_leaks.rs`

- [ ] **Step 1: Write the test**

```rust
// crates/forge-mem/tests/no_leaks.rs

use forge_mem::ExecutableBuffer;

/// `getrusage(RUSAGE_SELF).ru_maxrss` is a HIGH-WATER MARK, not a live RSS
/// reading -- it never decreases, even when `Drop`'s `munmap` genuinely
/// frees memory. So this test can't just check "RSS went down." Instead it
/// compares the high-water mark's growth RATE across two windows of
/// allocate/free cycles: flat growth after an initial warmup means the
/// munmap-on-Drop path is working; growth that keeps scaling with
/// iteration count means a real leak.
fn max_rss_kb() -> i64 {
    // SAFETY: `usage` is a valid, exclusively-owned local passed by
    // pointer for getrusage to fill in; RUSAGE_SELF has no preconditions.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        let rc = libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        assert_eq!(rc, 0, "getrusage failed: {}", std::io::Error::last_os_error());
        // macOS reports ru_maxrss in BYTES; Linux reports it in KB. Both
        // are only compared to each other WITHIN this one process/run
        // below, so the unit doesn't matter as long as it's consistent --
        // still normalize to a "kb-ish" unit for a readable failure
        // message if this ever fails.
        #[cfg(target_os = "macos")]
        {
            usage.ru_maxrss / 1024
        }
        #[cfg(not(target_os = "macos"))]
        {
            usage.ru_maxrss
        }
    }
}

fn allocate_write_free_cycle() {
    let mut buf = ExecutableBuffer::new(4096).expect("allocation should succeed");
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable().expect("make_executable should succeed");
    drop(buf);
}

#[test]
fn ten_thousand_allocate_free_cycles_do_not_leak() {
    // Warm up: let the allocator/OS settle into a steady state before
    // taking the first measurement (the first few hundred mmap/munmap
    // calls can grow RSS for reasons unrelated to leaking -- e.g. the
    // allocator's own bookkeeping pages).
    for _ in 0..500 {
        allocate_write_free_cycle();
    }
    let after_warmup = max_rss_kb();

    for _ in 0..10_000 {
        allocate_write_free_cycle();
    }
    let after_full_run = max_rss_kb();

    let growth = after_full_run - after_warmup;
    // A real per-buffer leak (even one page, 16KB on this platform) times
    // 10,000 iterations would be well over 100MB of growth. Allow a
    // generous fixed budget for legitimate one-time growth (allocator
    // metadata, page cache effects) without being so loose it'd miss an
    // actual leak.
    assert!(
        growth < 20_000, // 20MB
        "high-water-mark RSS grew by {growth}KB over 10,000 allocate/free cycles -- \
         this looks like a leak (Drop's munmap may not be running, or may be failing silently)"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p forge-mem --test no_leaks --release 2>&1 | tail -20`
Expected: passes. Run with `--release` since 10,500 mmap/munmap cycles in a debug build's test harness can be slow enough to be annoying, though correctness doesn't depend on the optimization level here. If it fails, do NOT just loosen the threshold — investigate whether `Drop` is genuinely being called (add a temporary `eprintln!` in `Drop::drop` if needed, then remove it) and whether `munmap`'s return value ever indicates failure.

- [ ] **Step 3: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 4: Commit**

```bash
git add crates/forge-mem/tests/no_leaks.rs
git commit -m "test(forge-mem): 10,000 allocate/free cycles leave RSS flat"
```

---

## Task 7: Windows stub

**Files:**
- Modify: `crates/forge-mem/src/lib.rs`

- [ ] **Step 1: Add an explicit, visible stub**

```rust
// crates/forge-mem/src/lib.rs — add near the top, after the doc comments on ExecutableBuffer/ProtState

#[cfg(windows)]
compile_error!(
    "forge-mem does not implement Windows yet (VirtualAlloc/VirtualProtect/FlushInstructionCache) -- \
     see CHECKLIST.md Phase 5 and the design doc's explicit scope decision to skip Windows until \
     there's a way to actually test it. Contributions welcome, but this must not silently compile \
     into a no-op or panic-at-runtime stub that looks like it works."
);
```

- [ ] **Step 2: Confirm this doesn't break the current build**

Run: `cargo check -p forge-mem` (on this machine, macOS — `cfg(windows)` is false here, so the `compile_error!` doesn't fire)
Expected: succeeds, unaffected.

- [ ] **Step 3: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 4: Commit**

```bash
git add crates/forge-mem/src/lib.rs
git commit -m "chore(forge-mem): explicit compile_error! stub for Windows (not implemented yet)"
```

---

## Task 8: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Document the Miri non-goal explicitly (don't let it be a silently-skipped checklist line)**

Add this to `crates/forge-mem/src/lib.rs`'s top-level doc comment (the `//!` block, creating one if it doesn't already exist above the `use std::io;` line):

```rust
//! **Miri is explicitly out of scope for this crate's CI.** Miri cannot
//! model raw `mmap`/`mprotect`/`sysconf` syscalls or a transmute-to a
//! function pointer followed by calling it -- there is no meaningful way
//! to run this crate's tests under Miri, so this is a documented decision,
//! not an oversight. Correctness here instead rests on: the macOS-AArch64
//! path being empirically tested (allocate/write/execute, W^X-enforcement
//! fork/SIGSEGV, and 10k-cycle leak tests all run for real on that
//! platform), and the generic-Unix path being cross-compile-checked only
//! (see the `platform` module in this file).
```

- [ ] **Step 2: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test passes, including `forge-mem`'s new tests (unit tests in `lib.rs`, `wx_enforcement.rs`, `no_leaks.rs`). No regressions in the pre-existing 150 tests from Phases 0-4.

- [ ] **Step 3: Cross-target compile checks (re-confirm Task 2's platform code still checks out after later tasks touched `lib.rs`)**

Run: `cargo check -p forge-mem --target x86_64-apple-darwin` and `cargo check -p forge-mem --target aarch64-unknown-linux-gnu`

- [ ] **Step 4: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 5: Format check**

Run: `cargo fmt --check`

- [ ] **Step 6: Confirm the day-one spike still works (Phase 5 shouldn't have touched it, but confirm)**

Run: `make spike`

- [ ] **Step 7: Report exit criteria status**

Confirm all 6 exit criteria from the design doc are met:
1. Basic execute test passes on this machine. ✅ (Task 1)
2. W^X-enforcement fork/`SIGSEGV` test passes on this machine. ✅ (Task 5)
3. Leak test shows flat high-water-mark growth. ✅ (Task 6)
4. `cargo test --workspace` green, clippy/fmt clean. ✅ (Steps 2, 4, 5)
5. x86-64/Linux-AArch64 paths compile-check cleanly, explicitly documented as untested. ✅ (Task 2, re-confirmed Step 3)
6. Windows is a visible, explicit stub. ✅ (Task 7)
