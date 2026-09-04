//! Executable memory management (ExecutableBuffer, W^X). See CHECKLIST.md Phase 5.
//!
//! **Miri is explicitly out of scope for this crate's CI.** Miri cannot
//! model raw `mmap`/`mprotect`/`sysconf` syscalls or a transmute-to a
//! function pointer followed by calling it -- there is no meaningful way
//! to run this crate's tests under Miri, so this is a documented decision,
//! not an oversight. Correctness here instead rests on: the macOS-AArch64
//! path being empirically tested (allocate/write/execute, W^X-enforcement
//! fork/SIGSEGV, and 10k-cycle leak tests all run for real on that
//! platform), and the generic-Unix path being cross-compile-checked only
//! (see the `platform` module in this file).

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

#[cfg(windows)]
compile_error!(
    "forge-mem does not implement Windows yet (VirtualAlloc/VirtualProtect/FlushInstructionCache) -- \
     see CHECKLIST.md Phase 5 and the design doc's explicit scope decision to skip Windows until \
     there's a way to actually test it. Contributions welcome, but this must not silently compile \
     into a no-op or panic-at-runtime stub that looks like it works."
);

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

    /// Restores write-protection and invalidates the icache when dropped --
    /// including on unwind, if the caller's closure in `write()` panics.
    /// `pthread_jit_write_protect_np` is a per-thread toggle affecting
    /// every `ExecutableBuffer` live on this thread, not just the one
    /// being written to right now, so leaving it disabled after a panic
    /// would silently break W^X for buffers this call never touched.
    struct WriteGuard {
        ptr: *mut u8,
        len: usize,
    }

    impl Drop for WriteGuard {
        fn drop(&mut self) {
            // SAFETY: restores write-protection and invalidates the
            // icache unconditionally, including on the unwind path --
            // ptr/len describe the same MAP_JIT mapping the enclosing
            // write() validated before constructing this guard.
            unsafe {
                pthread_jit_write_protect_np(1);
                sys_icache_invalidate(self.ptr as *mut libc::c_void, self.len);
            }
        }
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
                    io::Error::last_os_error().kind(),
                    format!(
                        "mmap MAP_JIT failed: {} -- is com.apple.security.cs.allow-jit present \
                         in the entitlements, and is the binary codesigned?",
                        io::Error::last_os_error()
                    ),
                ));
            }
            Ok(Self {
                ptr: ptr as *mut u8,
                len,
                state: ProtState::Writable,
            })
        }

        pub fn write<F: FnOnce(&mut [u8])>(&mut self, f: F) {
            // SAFETY: ptr/len come from a successful MAP_JIT mmap in new().
            // pthread_jit_write_protect_np(0) grants THIS thread write
            // access to MAP_JIT pages (mprotect cannot be used on them --
            // it returns EACCES); f only ever sees a correctly-bounded
            // slice. The WriteGuard constructed below revokes write access
            // and invalidates the icache when it drops -- on the normal
            // return path AND on unwind if `f` panics -- so no other
            // thread can observe a state where the page is both writable
            // and stale-icache at once, and a panicking `f` can't leave
            // this thread's write-protection toggle disabled.
            unsafe {
                pthread_jit_write_protect_np(0);
                let guard = WriteGuard {
                    ptr: self.ptr,
                    len: self.len,
                };
                let slice = std::slice::from_raw_parts_mut(self.ptr, self.len);
                f(slice);
                drop(guard);
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

        /// Resets the buffer's nominal state to `Writable` for reuse by
        /// `CodeCache`. No syscall needed on this platform -- `write()`
        /// always does its own per-call protect dance regardless of the
        /// buffer's nominal state, so there's nothing to actually
        /// "unprotect" up front. Always succeeds; returns `io::Result<()>`
        /// only to keep a uniform signature with the generic-Unix platform
        /// module, where this CAN fail.
        pub fn reset_to_writable(&mut self) -> io::Result<()> {
            self.state = ProtState::Writable;
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
            Ok(Self {
                ptr: ptr as *mut u8,
                len,
                state: ProtState::Writable,
            })
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
            // SAFETY: ptr/len describe this buffer's own mapping, which is
            // still valid and mapped at this point (new()'s mmap succeeded,
            // and drop() -- the only thing that unmaps it -- can't have run
            // yet since we're inside a &mut self method).
            unsafe {
                clear_icache_if_needed(self.ptr, self.len);
            }
        }

        pub fn make_executable(&mut self) -> io::Result<()> {
            // SAFETY: ptr/len are page-aligned, from a successful mmap.
            let rc = unsafe {
                libc::mprotect(
                    self.ptr as *mut libc::c_void,
                    self.len,
                    libc::PROT_READ | libc::PROT_EXEC,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            self.state = ProtState::Executable;
            Ok(())
        }

        /// Resets the buffer back to a real RW mapping for reuse by
        /// `CodeCache` -- unlike the macOS-AArch64 path, this platform's
        /// `write()` requires the page to genuinely be in `Writable` state
        /// already (see its `debug_assert_eq!`), so this needs a real
        /// `mprotect` call, not just a state-field reset.
        pub fn reset_to_writable(&mut self) -> io::Result<()> {
            // SAFETY: ptr/len are page-aligned, from a successful mmap.
            let rc = unsafe {
                libc::mprotect(
                    self.ptr as *mut libc::c_void,
                    self.len,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            self.state = ProtState::Writable;
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
    use std::sync::OnceLock;

    #[cfg(target_arch = "aarch64")]
    fn ctr_el0() -> u64 {
        let ctr_el0: u64;
        // SAFETY: MRS from CTR_EL0 is a read-only, unprivileged system
        // register read with no preconditions -- available at EL0 on all
        // AArch64 cores.
        unsafe {
            std::arch::asm!("mrs {0}, ctr_el0", out(reg) ctr_el0);
        }
        ctr_el0
    }

    /// Instruction-cache line size in bytes, read from `CTR_EL0.IminLine`
    /// (bits [3:0], expressed in words) rather than assumed -- cache line
    /// size is NOT architecturally guaranteed to be any particular value,
    /// which is exactly why this register exists and why glibc/Linux/V8/JSC
    /// all query it at runtime instead of hardcoding a constant. Cached
    /// after the first read since it cannot change at runtime.
    #[cfg(target_arch = "aarch64")]
    fn icache_line_size() -> usize {
        static LINE: OnceLock<usize> = OnceLock::new();
        *LINE.get_or_init(|| {
            let imin_line = (ctr_el0() & 0xF) as u32;
            4usize << imin_line
        })
    }

    /// Data-cache line size in bytes, read from `CTR_EL0.DminLine` (bits
    /// [19:16], expressed in words). Kept separate from
    /// `icache_line_size()` -- the two can differ on some cores, even
    /// though on most real hardware both are 64 bytes.
    #[cfg(target_arch = "aarch64")]
    fn dcache_line_size() -> usize {
        static LINE: OnceLock<usize> = OnceLock::new();
        *LINE.get_or_init(|| {
            let dmin_line = ((ctr_el0() >> 16) & 0xF) as u32;
            4usize << dmin_line
        })
    }

    /// # Safety
    /// `[ptr, ptr + len)` must be a valid, currently-mapped region that the
    /// caller exclusively owns. The `dc`/`ic` instructions issued here can
    /// architecturally fault on a bad or unmapped virtual address.
    #[cfg(target_arch = "aarch64")]
    unsafe fn clear_icache_if_needed(ptr: *mut u8, len: usize) {
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
        // checked only. The stride/alignment for each loop is read from
        // CTR_EL0 at runtime (see icache_line_size()/dcache_line_size())
        // rather than assumed -- a hardcoded guess would silently skip
        // cache lines and leave stale icache entries on any core whose
        // real line size is smaller than the guess.
        //
        // SAFETY: caller guarantees ptr/len describe a valid, currently-
        // mapped region; the asm blocks only read `addr` and issue
        // architecturally-defined cache-maintenance instructions -- no
        // memory is written by this function.
        unsafe {
            let start = ptr as usize;
            let end = start + len;

            let dline = dcache_line_size();
            let mut addr = start & !(dline - 1);
            while addr < end {
                std::arch::asm!("dc cvau, {0}", in(reg) addr);
                addr += dline;
            }
            std::arch::asm!("dsb ish");

            let iline = icache_line_size();
            let mut addr = start & !(iline - 1);
            while addr < end {
                std::arch::asm!("ic ivau, {0}", in(reg) addr);
                addr += iline;
            }
            std::arch::asm!("dsb ish");
            std::arch::asm!("isb");
        }
    }

    /// # Safety
    /// `[ptr, ptr + len)` must be a valid, currently-mapped region that the
    /// caller exclusively owns (matches the AArch64 variant's contract even
    /// though this variant does nothing with it, so callers don't need a
    /// `#[cfg]` split at the call site).
    #[cfg(not(target_arch = "aarch64"))]
    unsafe fn clear_icache_if_needed(_ptr: *mut u8, _len: usize) {
        // x86-64: the icache is hardware-coherent with the dcache. Nothing
        // to do -- this empty fn exists so `write()`'s call site doesn't
        // need its own `#[cfg]` split.
    }
}

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
        // A real assert, not debug-only: catching a never-made-executable
        // buffer here, at construction, is strictly better than deferring
        // to the first call1/call2/call_n (which check the same thing) --
        // it fails at the actual site of the caller's mistake.
        assert_eq!(
            buf.state(),
            ProtState::Executable,
            "cannot build a CompiledExpr from a buffer that was never made executable"
        );
        Self { buf, arity }
    }

    pub fn call1(&self, x: f64) -> f64 {
        assert_eq!(
            self.arity, 1,
            "arity mismatch: compiled for {} argument(s), called via call1",
            self.arity
        );
        // A real assert, not debug-only -- see the comment on call_n's
        // state check for why this matters on macOS AArch64 specifically.
        assert_eq!(
            self.buf.state(),
            ProtState::Executable,
            "cannot call a CompiledExpr whose buffer was never made executable"
        );
        // SAFETY: arity and state checked above (both real asserts, so this
        // holds in release builds too); the buffer contains a complete
        // function honoring AAPCS64/SysV's fn(f64) -> f64 convention by
        // construction -- this phase's own tests are the only thing writing
        // bytes into any buffer this type wraps (no compiler exists yet to
        // violate that contract).
        let f: unsafe extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x) }
    }

    pub fn call2(&self, x: f64, y: f64) -> f64 {
        assert_eq!(
            self.arity, 2,
            "arity mismatch: compiled for {} argument(s), called via call2",
            self.arity
        );
        // A real assert, not debug-only -- see the comment on call_n's
        // state check for why this matters on macOS AArch64 specifically.
        assert_eq!(
            self.buf.state(),
            ProtState::Executable,
            "cannot call a CompiledExpr whose buffer was never made executable"
        );
        // SAFETY: same as call1, for fn(f64, f64) -> f64.
        let f: unsafe extern "C" fn(f64, f64) -> f64 =
            unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(x, y) }
    }

    /// Calls a compiled function using the platform C ABI's scalar f64
    /// argument registers. This is distinct from [`Self::call_n`], whose
    /// pointer argument is useful for array-style entry points but is not the
    /// ABI emitted for ordinary scalar parameters by `forge-emit`.
    pub fn call_args(&self, args: &[f64]) -> f64 {
        assert_eq!(
            args.len(),
            self.arity,
            "arity mismatch: compiled for {} argument(s), called with {}",
            self.arity,
            args.len()
        );
        assert!(
            args.len() <= 8,
            "scalar f64 calls support at most 8 ABI register arguments"
        );
        assert_eq!(
            self.buf.state(),
            ProtState::Executable,
            "cannot call a CompiledExpr whose buffer was never made executable"
        );

        // SAFETY: the arity and executable state are checked above, and each
        // function-pointer type below exactly matches the platform C ABI used
        // by the scalar emitter for that argument count. `args` has already
        // been checked to contain every argument passed to the callee.
        unsafe {
            match args {
                [] => {
                    let f: unsafe extern "C" fn() -> f64 = std::mem::transmute(self.buf.as_ptr());
                    f()
                }
                [a] => {
                    let f: unsafe extern "C" fn(f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a)
                }
                [a, b] => {
                    let f: unsafe extern "C" fn(f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b)
                }
                [a, b, c] => {
                    let f: unsafe extern "C" fn(f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c)
                }
                [a, b, c, d] => {
                    let f: unsafe extern "C" fn(f64, f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c, *d)
                }
                [a, b, c, d, e] => {
                    let f: unsafe extern "C" fn(f64, f64, f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c, *d, *e)
                }
                [a, b, c, d, e, g] => {
                    let f: unsafe extern "C" fn(f64, f64, f64, f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c, *d, *e, *g)
                }
                [a, b, c, d, e, g, h] => {
                    let f: unsafe extern "C" fn(f64, f64, f64, f64, f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c, *d, *e, *g, *h)
                }
                [a, b, c, d, e, g, h, i] => {
                    let f: unsafe extern "C" fn(f64, f64, f64, f64, f64, f64, f64, f64) -> f64 =
                        std::mem::transmute(self.buf.as_ptr());
                    f(*a, *b, *c, *d, *e, *g, *h, *i)
                }
                _ => unreachable!("argument count was limited to eight above"),
            }
        }
    }

    pub fn call_n(&self, args: &[f64]) -> f64 {
        // >= rather than == : the compiled function is only guaranteed to
        // read the first `arity` elements through the raw pointer below, so
        // a caller-supplied slice longer than `arity` is safe (the extra
        // elements are simply unread) -- only a slice SHORTER than `arity`
        // would let the callee read out of bounds.
        assert!(
            args.len() >= self.arity,
            "arity mismatch: compiled for {} argument(s), called via call_n with only {}",
            self.arity,
            args.len()
        );
        // A real assert, not debug-only. On the generic-Unix platform this
        // check is merely a nicer panic message than the SIGSEGV a still-
        // Writable buffer's real (PROT_READ|WRITE, no PROT_EXEC) mapping
        // would raise anyway. But on macOS AArch64 -- the only platform
        // this crate is actually tested on -- `ExecutableBuffer::new()`
        // maps PROT_READ|WRITE|EXEC unconditionally via MAP_JIT, and
        // `make_executable()` there is a pure state-field flip with no
        // syscall backing it. So on THIS platform, this assert is the
        // entire protection against transmuting-and-calling into an
        // unfinished or partially-written buffer -- there is no OS-level
        // backstop to fall back on, which is why this can't be a
        // debug-only check.
        assert_eq!(
            self.buf.state(),
            ProtState::Executable,
            "cannot call a CompiledExpr whose buffer was never made executable"
        );
        // SAFETY: same as call1, for fn(*const f64) -> f64; `args` outlives
        // this call and its pointer is valid for at least `self.arity`
        // reads, which is what the arity check above guarantees (and all
        // the callee is permitted to read).
        let f: unsafe extern "C" fn(*const f64) -> f64 =
            unsafe { std::mem::transmute(self.buf.as_ptr()) };
        unsafe { f(args.as_ptr()) }
    }
}

/// A minimal free-list, not a size-class allocator -- there's no real
/// compiler pipeline yet to stress this with varied allocation patterns.
/// Reuses `mmap`'d pages across compilations instead of syscalling on every
/// single one.
#[derive(Default)]
pub struct CodeCache {
    free: Vec<ExecutableBuffer>,
}

impl CodeCache {
    /// Returns a buffer in `Writable` state, never `Executable` -- call
    /// `write()` then `make_executable()` on it before wrapping it in a
    /// `CompiledExpr`, whose `from_buffer` asserts on a non-`Executable`
    /// buffer.
    pub fn acquire(&mut self, min_size: usize) -> io::Result<ExecutableBuffer> {
        if let Some(pos) = self.free.iter().position(|b| b.len() >= min_size) {
            return Ok(self.free.remove(pos));
        }
        ExecutableBuffer::new(min_size)
    }

    pub fn release(&mut self, mut buf: ExecutableBuffer) {
        buf.reset_to_writable().expect(
            "failed to reset a previously-valid ExecutableBuffer back to writable during release",
        );
        self.free.push(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_i64_bytes() -> &'static [u8] {
        #[cfg(target_arch = "aarch64")]
        {
            &[0xC0, 0x03, 0x5F, 0xD6]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &[0x48, 0x89, 0xF8, 0xC3]
        }
    }

    fn identity_f64_bytes() -> &'static [u8] {
        #[cfg(target_arch = "aarch64")]
        {
            &[0xC0, 0x03, 0x5F, 0xD6]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &[0xC3]
        }
    }

    fn add_f64_bytes() -> &'static [u8] {
        #[cfg(target_arch = "aarch64")]
        {
            &[0x00, 0x28, 0x61, 0x1E, 0xC0, 0x03, 0x5F, 0xD6]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &[0xF2, 0x0F, 0x58, 0xC1, 0xC3]
        }
    }

    fn load_f64_bytes() -> &'static [u8] {
        #[cfg(target_arch = "aarch64")]
        {
            &[0x00, 0x00, 0x40, 0xFD, 0xC0, 0x03, 0x5F, 0xD6]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &[0xF2, 0x0F, 0x10, 0x07, 0xC3]
        }
    }

    #[test]
    fn allocate_write_execute_roundtrip() {
        let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");
        assert_eq!(buf.state(), ProtState::Writable);

        // Use the platform-specific identity function so this smoke test
        // exercises executable memory on both AArch64 and x86-64.
        buf.write(|mem| {
            let bytes = identity_i64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });

        buf.make_executable()
            .expect("make_executable should succeed");
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
        assert!(!buf.is_empty());
    }

    #[test]
    fn size_is_rounded_up_to_a_page_multiple() {
        let page = page_size();
        let buf = ExecutableBuffer::new(1).expect("allocation should succeed");
        assert_eq!(buf.len() % page, 0);
        assert!(buf.len() >= page);
    }

    // The panic-safety regression test for write()'s unwind path lives in
    // tests/write_panic_protection.rs, not here: it needs fork(), and
    // forking safely requires being the only test running in its process
    // (see that file's doc comment) -- a guarantee this multi-test `--lib`
    // binary can't give it.

    #[test]
    fn call1_identity() {
        let mut buf = ExecutableBuffer::new(64).unwrap();
        // On both supported architectures the first f64 argument and return
        // value use the same register, so the identity body is just `ret`.
        /* Independently verified: compiled
        // `extern "C" fn identity(x: f64) -> f64 { x }` with
        // `rustc --crate-type=lib -O --emit=obj` and disassembled with
        // `otool -tv` / `otool -s __TEXT __text -x` -- produced a bare `ret`,
        // raw word `d65f03c0` (little-endian bytes C0 03 5F D6), matching
        // exactly. */
        buf.write(|mem| {
            let bytes = identity_f64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        buf.make_executable().unwrap();
        let compiled = CompiledExpr::from_buffer(buf, 1);
        assert_eq!(compiled.call_args(&[3.5]), 3.5);
    }

    #[test]
    fn call2_add() {
        let mut buf = ExecutableBuffer::new(64).unwrap();
        // fn(f64, f64) -> f64 { x + y }, using the platform's scalar f64
        // add instruction and ABI registers.
        /* Independently verified: compiled
        // `extern "C" fn add(x: f64, y: f64) -> f64 { x + y }` with
        // `rustc --crate-type=lib -O --emit=obj` and disassembled with
        // `otool -tv` / `otool -s __TEXT __text -x` -- produced
        // `fadd d0, d0, d1; ret`, raw words `1e612800 d65f03c0`
        // (little-endian bytes 00 28 61 1E / C0 03 5F D6), matching exactly. */
        buf.write(|mem| {
            let bytes = add_f64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        buf.make_executable().unwrap();
        let compiled = CompiledExpr::from_buffer(buf, 2);
        assert_eq!(compiled.call_args(&[2.0, 3.0]), 5.0);
    }

    #[test]
    fn call_n_reads_first_element() {
        let mut buf = ExecutableBuffer::new(64).unwrap();
        // fn(*const f64) -> f64 { *ptr }, using the platform's pointer
        // argument register and scalar floating-point load instruction.
        buf.write(|mem| {
            let bytes = load_f64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        buf.make_executable().unwrap();
        let compiled = CompiledExpr::from_buffer(buf, 1);
        assert_eq!(compiled.call_n(&[9.5, 100.0]), 9.5);
    }

    #[test]
    #[should_panic(expected = "arity mismatch")]
    fn call1_panics_on_arity_mismatch() {
        let mut buf = ExecutableBuffer::new(64).unwrap();
        buf.write(|mem| {
            let bytes = identity_i64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        buf.make_executable().unwrap();
        let compiled = CompiledExpr::from_buffer(buf, 2); // arity 2, but we call call1
        compiled.call1(1.0);
    }

    // NOTE: this exercises the "call1 on a buffer that was never made
    // executable" scenario end to end -- but the panic actually fires
    // inside `from_buffer`, not inside `call1`. `from_buffer` now performs
    // the very same state check up front (see its doc comment), so there is
    // no way to reach a live `CompiledExpr` wrapping a still-Writable
    // buffer through the public API at all -- the checks inside
    // call1/call2/call_n are unreachable-in-practice defense in depth, not
    // something a black-box test can trigger independently of this one.
    #[test]
    #[should_panic(expected = "never made executable")]
    fn from_buffer_panics_on_a_still_writable_buffer() {
        let mut buf = ExecutableBuffer::new(64).unwrap();
        buf.write(|mem| {
            let bytes = identity_i64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        // Deliberately never call buf.make_executable().
        let compiled = CompiledExpr::from_buffer(buf, 1);
        compiled.call1(1.0);
    }

    #[test]
    fn code_cache_reuses_a_released_buffer() {
        let mut cache = CodeCache::default();
        let buf1 = cache.acquire(64).unwrap();
        let ptr1 = buf1.as_ptr();
        cache.release(buf1);

        let buf2 = cache.acquire(64).unwrap();
        assert_eq!(
            buf2.as_ptr(),
            ptr1,
            "acquire() should reuse the released buffer's mapping, not allocate a fresh one"
        );
    }

    #[test]
    fn code_cache_allocates_fresh_when_nothing_reusable_is_large_enough() {
        let mut cache = CodeCache::default();
        let small = cache.acquire(64).unwrap();
        let small_ptr = small.as_ptr();
        cache.release(small);

        let big = cache.acquire(page_size() * 4).unwrap();
        assert_ne!(
            big.as_ptr(),
            small_ptr,
            "a too-small released buffer must not be reused for a bigger request"
        );
    }

    #[test]
    fn released_buffer_is_writable_again() {
        let mut cache = CodeCache::default();
        let mut buf = cache.acquire(64).unwrap();
        buf.write(|mem| {
            let bytes = identity_i64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        buf.make_executable().unwrap();
        cache.release(buf);

        let mut reused = cache.acquire(64).unwrap();
        assert_eq!(
            reused.state(),
            ProtState::Writable,
            "a reused buffer must come back in a writable state, ready for a fresh write()"
        );
        // Confirm it's genuinely usable, not just claiming to be writable:
        reused.write(|mem| {
            let bytes = identity_i64_bytes();
            mem[..bytes.len()].copy_from_slice(bytes);
        });
        reused.make_executable().unwrap();
    }
}
