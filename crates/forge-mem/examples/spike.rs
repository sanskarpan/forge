// crates/forge-mem/examples/spike.rs

//! Day-one proof that this machine can allocate W^X memory, write real
//! machine code into it, and execute it. If this doesn't run, nothing else
//! in the project matters — fix the platform setup before writing another
//! line of forge.

fn main() {
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let mem = {
            // Apple Silicon: MAP_JIT is required, and so is the
            // com.apple.security.cs.allow-jit entitlement on a signed binary.
            let p = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_JIT,
                -1,
                0,
            ) as *mut u8;
            assert_ne!(p as isize, -1, "mmap MAP_JIT failed: {} — is the binary codesigned with entitlements.plist?", std::io::Error::last_os_error());
            p
        };

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        let mem = {
            let p = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            ) as *mut u8;
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
            pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len());
            pthread_jit_write_protect_np(1);
            sys_icache_invalidate(mem as *mut libc::c_void, page);
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len());
            assert_eq!(libc::mprotect(mem as _, page, libc::PROT_READ | libc::PROT_EXEC), 0,
                "mprotect failed: {}", std::io::Error::last_os_error());
        }

        let f: extern "C" fn(i64) -> i64 = std::mem::transmute(mem);
        assert_eq!(f(42), 42);
        println!("JIT works: f(42) = {}", f(42));
    }
}
