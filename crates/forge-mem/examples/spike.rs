// crates/forge-mem/examples/spike.rs

//! Day-one proof that this machine can allocate W^X memory, write real
//! machine code into it, and execute it. If this doesn't run, nothing else
//! in the project matters — fix the platform setup before writing another
//! line of forge.

fn main() {
    // SAFETY: sysconf(_SC_PAGESIZE) has no preconditions — it just reads a
    // kernel-reported constant.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let mem = {
        // Apple Silicon: MAP_JIT is required, and so is the
        // com.apple.security.cs.allow-jit entitlement on a signed binary.
        //
        // SAFETY: null hint (let the kernel choose the address), `page` is a
        // non-zero, page-size-multiple length, and the prot/flags
        // combination (RWX + PRIVATE|ANONYMOUS|JIT) is a valid mmap
        // argument on this platform. fd/offset are ignored for anonymous
        // mappings. Failure returns MAP_FAILED ((void*)-1), which we check
        // immediately below rather than dereferencing a bad pointer.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT,
                -1,
                0,
            )
        } as *mut u8;
        assert_ne!(p as isize, -1, "mmap MAP_JIT failed: {} — is the binary codesigned with entitlements.plist?", std::io::Error::last_os_error());
        p
    };

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let mem = {
        // SAFETY: null hint, `page` is a non-zero, page-size-multiple
        // length, and PROT_READ|PROT_WRITE with MAP_PRIVATE|MAP_ANONYMOUS
        // is a standard, always-valid mmap argument combination. fd/offset
        // are ignored for anonymous mappings. Failure returns MAP_FAILED,
        // checked immediately below.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        } as *mut u8;
        assert_ne!(p as isize, -1, "mmap failed: {}", std::io::Error::last_os_error());
        p
    };

    // The machine code payload is architecture-specific: we're emitting
    // real opcodes for the host CPU, not a portable byte sequence.
    //
    // x86-64: `mov rax, rdi; ret` (48 89 F8 C3) — explicitly moves the
    // first argument register (rdi) into the return register (rax).
    //
    // AArch64: the identity function is just `ret` (encoded C0 03 5F D6,
    // i.e. instruction word 0xD65F03C0). AArch64's calling convention
    // already uses x0 for both the first argument and the return value,
    // so there is no register move to perform — the incoming x0 is the
    // outgoing x0 by construction. A bare `ret` is therefore the correct
    // (and only sensible) 4-byte identity function here; reusing the
    // x86-64 bytes on this host would decode as garbage AArch64
    // instructions and crash with SIGILL.
    #[cfg(target_arch = "x86_64")]
    let code = [0x48u8, 0x89, 0xF8, 0xC3];

    #[cfg(target_arch = "aarch64")]
    let code = [0xC0u8, 0x03, 0x5F, 0xD6];

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        extern "C" {
            fn pthread_jit_write_protect_np(enabled: libc::c_int);
            fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
        }
        // SAFETY: this thread's JIT-mapped pages (including `mem`, which
        // came from a successful MAP_JIT mmap above) are currently
        // executable-but-not-writable; toggling write-protect off here
        // makes them writable-but-not-executable for this thread only,
        // which is required before the copy_nonoverlapping below is
        // allowed to touch `mem` at all. No preconditions beyond running
        // on Apple Silicon, which this cfg arm guarantees.
        unsafe { pthread_jit_write_protect_np(0) };
        // SAFETY: `mem` is a valid, writable (write-protect just disabled)
        // allocation of at least `page` bytes from the mmap above; `code`
        // is a 4-byte stack array, so `code.len() == 4 <= page`; source and
        // destination do not overlap (one is heap/mmap, the other is a
        // local array).
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len()) };
        // SAFETY: reverses the write-protect toggle above; same
        // preconditions (this thread, this JIT-mapped region). After this
        // call `mem` is executable-but-not-writable again, which is
        // required before we transmute it to a function pointer and call
        // it below.
        unsafe { pthread_jit_write_protect_np(1) };
        // SAFETY: `mem` is a valid pointer to a live mapping of at least
        // `page` bytes (from the mmap above), and `page` is the exact
        // length of that mapping, so the instruction-cache invalidation
        // range does not exceed the allocation. Required on Apple Silicon
        // because the CPU's instruction and data caches are not coherent
        // for freshly-written code — without this, the core executing
        // `f(42)` below could fetch stale (pre-write) icache contents.
        unsafe { sys_icache_invalidate(mem as *mut libc::c_void, page) };
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        // SAFETY: `mem` is a valid, writable allocation of at least `page`
        // bytes from the mmap above; `code` is a 4-byte stack array, so
        // `code.len() == 4 <= page`; source and destination do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len()) };
        // SAFETY: `mem`/`page` came from the successful mmap above and are
        // page-aligned (mmap always returns page-aligned addresses), which
        // mprotect requires. Flipping to READ|EXEC (and dropping WRITE) is
        // what makes this W^X: the page is never writable and executable
        // at the same time.
        let rc = unsafe { libc::mprotect(mem as _, page, libc::PROT_READ | libc::PROT_EXEC) };
        assert_eq!(rc, 0, "mprotect failed: {}", std::io::Error::last_os_error());
    }

    // SAFETY: this is the single most dangerous line in the file. `mem`
    // points to a page we just wrote a complete, valid 4-byte function
    // body into (either `mov rax, rdi; ret` or a bare AArch64 `ret`,
    // matching the host's actual instruction set per the cfg above) and
    // then made executable-and-not-writable (via mprotect, or via the
    // pthread_jit_write_protect_np(1) toggle on Apple Silicon) — so the
    // bytes at `mem` are guaranteed to be exactly the opcodes we chose,
    // not stale or partially-written data. The pointer is non-null (mmap
    // success was asserted above) and the mapping remains alive for the
    // rest of `main`, which is the entire lifetime `f` is used for. The
    // transmuted signature, `extern "C" fn(i64) -> i64`, matches the
    // System V / AAPCS64 calling convention our 4 bytes actually implement
    // (single integer arg in rdi/x0, single integer return in rax/x0).
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(mem) };
    let result = f(42);
    assert_eq!(result, 42);
    println!("JIT works: f(42) = {}", result);
}
