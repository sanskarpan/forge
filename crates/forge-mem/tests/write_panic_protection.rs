// crates/forge-mem/tests/write_panic_protection.rs

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use forge_mem::ExecutableBuffer;

/// Regression test for a panic-safety bug in `write()`: if the caller's
/// closure panics partway through a `write()` call, this thread's JIT
/// write-protect toggle must still end up restored to "protected" once the
/// panic has unwound past `write()` -- not left permanently "unprotected,"
/// which would silently break W^X for every `ExecutableBuffer` used on this
/// thread afterward (`pthread_jit_write_protect_np` is a per-thread
/// toggle, not tied to any one buffer).
///
/// This can't be checked by inspecting `ExecutableBuffer`'s own state --
/// there's no field recording whether this thread's hardware write-protect
/// toggle is currently on, and attempting an illegal write from the test
/// process itself would be UB if the bug is present (a silent, partial
/// overwrite of the test binary's own mapping, not a clean failure). So
/// this uses the same fork()+"killed by a signal" technique the design doc
/// prescribes for W^X enforcement, with one crucial ordering difference
/// from that pattern, found only by empirically checking this test against
/// the pre-fix code before trusting it (see the note below): fork()
/// itself unconditionally resets a freshly forked child's write-protect
/// toggle to "protected," regardless of what the parent thread's toggle
/// was at fork time -- confirmed on this machine by probing
/// `pthread_jit_write_protect_np` directly across a fork with the toggle
/// deliberately left enabled beforehand; the child still faulted on an
/// otherwise-writable page. So triggering the panic in the PARENT and only
/// forking afterward (the first version of this test did exactly that)
/// proves nothing: the child would come up "protected" either way, whether
/// or not `write()`'s unwind path is actually correct.
///
/// The fix: fork() FIRST, before anything that touches the toggle, then do
/// the entire panic-mid-write-and-probe sequence inside the CHILD, on its
/// one thread, with no further fork() in between. That way the raw probe
/// write genuinely observes the toggle state `write()`'s unwind path left
/// behind, not a state fork() reset out from under it.
///
/// This test is deliberately its OWN test binary (not folded into
/// `src/lib.rs`'s `#[cfg(test)] mod tests`, which has other tests that run
/// concurrently on separate threads within the same process under `cargo
/// test --lib`): forking while unrelated threads are concurrently doing
/// mmap/munmap/pthread_jit_write_protect_np calls is a correctness hazard
/// independent of what's under test here. Being the only test in this
/// binary means fork() only ever has to account for this one thread's
/// state, exactly like `wx_enforcement.rs`'s use of the same technique.
#[test]
fn a_panicking_write_leaves_the_thread_write_protected() {
    let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");
    let ptr = buf.as_ptr() as *mut u8;

    // SAFETY: fork() has no preconditions beyond "don't do this in a
    // multi-threaded process without knowing what you're doing" -- this
    // test binary contains only this one test, so no other thread is
    // running at fork time.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());

    if pid == 0 {
        // Child: everything under test happens here, on this single
        // thread, with no further fork() between the panic and the probe
        // write below -- see the doc comment above for why that ordering
        // matters. Only async-signal-safe operations happen before this
        // process either faults or exits (no allocation beyond what
        // catch_unwind/write() themselves need internally, no locking, no
        // println!), avoiding the classic post-fork deadlock hazards.

        // Trigger the panic path and swallow it -- mirrors a real caller
        // whose closure panics mid-write. write()'s own unwind-safety is
        // what's under test, not propagation of the panic itself.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            buf.write(|_mem| panic!("boom"));
        }))
        .is_err();
        if !panicked {
            // The test's own premise didn't hold -- the closure somehow
            // didn't panic. Exit with a code distinct from both the
            // "toggle left enabled" (123) and "killed by signal" outcomes
            // so a broken premise doesn't get misread as either.
            std::process::exit(124);
        }

        // Attempt a raw write directly into the buffer's memory, bypassing
        // the safe write() API entirely -- the whole point of this test is
        // to observe whether THIS thread's write-protect toggle was left
        // enabled by the panic that just unwound through write() above. If
        // the WriteGuard fix is doing its job, the toggle was restored to
        // "protected" during unwind and this write faults; if the fix
        // regresses (e.g. someone reintroduces an unguarded
        // protect/write/reprotect sequence), the toggle is still
        // "unprotected" and this write silently succeeds instead.
        //
        // SAFETY: deliberately violating memory safety to observe whether
        // the OS stops us -- `ptr` points at a real page from this
        // process's own MAP_JIT mapping; this is the entire point of the
        // test.
        unsafe {
            std::ptr::write_volatile(ptr, 0xFFu8);
        }
        // If we reach here, the write succeeded -- the bug is present.
        // Exit with a distinct, non-zero code so the parent can tell
        // "wrote successfully" apart from "was killed by a signal."
        std::process::exit(123);
    }

    // Parent: wait for the child and confirm it died from a signal (the
    // toggle was genuinely restored to "protected" after the panic) rather
    // than exiting normally with code 123 (the toggle was left
    // "unprotected," meaning WriteGuard's Drop did not run on unwind) or
    // 124 (the test's own premise -- that the closure panics -- didn't
    // hold, which would need investigating separately).
    let mut status: libc::c_int = 0;
    // SAFETY: pid is the value fork() just returned to us in the parent
    // branch (guaranteed > 0 here since we're not in the pid==0 branch);
    // `status` is a valid, exclusively-owned local passed by pointer for
    // waitpid to write into.
    let wait_result = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(
        wait_result,
        pid,
        "waitpid failed: {}",
        std::io::Error::last_os_error()
    );

    if libc::WIFEXITED(status) {
        let code = libc::WEXITSTATUS(status);
        assert_ne!(
            code, 124,
            "test premise failed: the write() closure did not panic as expected"
        );
    }

    let signaled = libc::WIFSIGNALED(status);
    assert!(
        signaled,
        "expected the child to be killed by a signal (write-protection was \
         restored after the panic), but it exited normally with status {status} \
         -- this means write()'s panic-unwind path is leaving this thread's \
         JIT write-protect toggle disabled (the WriteGuard fix has regressed)"
    );
}
