//! Executable memory management (ExecutableBuffer, W^X). See CHECKLIST.md Phase 5.

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
                return Err(io::Error::other(format!(
                    "mmap MAP_JIT failed: {} -- is com.apple.security.cs.allow-jit present \
                     in the entitlements, and is the binary codesigned?",
                    io::Error::last_os_error()
                )));
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
