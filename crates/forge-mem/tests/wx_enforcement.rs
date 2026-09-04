// crates/forge-mem/tests/wx_enforcement.rs

use forge_mem::ExecutableBuffer;

/// Proves W^X is genuinely enforced by the OS, not just "we didn't call
/// mprotect(W) after make_executable and got lucky" -- and, more
/// specifically, that `ExecutableBuffer::write()`'s own protect/
/// write/reprotect sequence is what does the enforcing, not some incidental
/// property of `fork()` itself.
///
/// The entire `write()` -> `make_executable()` -> illegal-write-attempt
/// sequence happens INSIDE the forked child, after `fork()`, not before it.
/// An earlier version of this test called `write()`/`make_executable()` in
/// the PARENT and only forked afterward to attempt the illegal write --
/// that version passed even with `WriteGuard::drop()`'s
/// `pthread_jit_write_protect_np(1)` call deleted (confirmed empirically by
/// temporarily removing it and rerunning), because on macOS AArch64 a
/// freshly forked child's thread starts with the JIT write-protect toggle
/// "protected" regardless of the parent thread's toggle state at fork time
/// (the same fork-resets-the-toggle behavior documented at length in
/// `write_panic_protection.rs`, from Task 1). So a test that does the
/// protect/write/reprotect dance in the parent and only forks afterward is
/// really just testing "does a freshly-forked thread default to
/// protected," which is true independent of whether `write()`/
/// `make_executable()` do anything correct at all -- it doesn't exercise
/// this crate's own reprotection logic in the slightest.
///
/// The fix: fork() FIRST, before anything touches the write-protect toggle,
/// then do write() (which internally protects, writes, reprotects, and
/// invalidates the icache via `WriteGuard`), make_executable(), and the
/// illegal-write probe all inside the CHILD, on its one thread, with no
/// further fork() in between. That way the probe write genuinely observes
/// the toggle state `write()`'s own reprotection left behind, not a state
/// fork() reset out from under it -- if `WriteGuard::drop()`'s
/// reprotection call regresses (e.g. someone deletes or breaks it), this
/// test now actually fails instead of passing for the wrong reason.
///
/// Running the illegal write in a forked child (not the test process
/// itself) still matters for the same reason as before: a genuine W^X
/// violation only kills the disposable child, not the whole test binary,
/// and an illegal write from this process directly would be UB.
#[test]
#[ignore = "requires a properly signed process with com.apple.security.cs.allow-jit; run explicitly on an entitled Apple Silicon binary"]
fn writing_to_an_executable_page_segfaults() {
    // Only the mmap (via new()) happens in the parent -- it doesn't touch
    // any per-thread protect toggle, so sharing it with the child via
    // fork()'s copy-on-write address space is safe and doesn't undermine
    // what this test is proving.
    let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");

    // SAFETY: fork() has no preconditions beyond "don't do this in a
    // multi-threaded process without knowing what you're doing" -- this
    // test binary's threading is entirely under our control here, and
    // only async-signal-safe operations happen in the child before it
    // either faults or exits, avoiding the classic post-fork deadlock
    // hazards (no allocation beyond what write()/make_executable()
    // themselves need internally, no locking, no println! in the child).
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());

    if pid == 0 {
        // Child: everything under test happens here, on this single
        // thread, with no further fork() between write() and the illegal-
        // write probe below -- see the doc comment above for why that
        // ordering matters.

        // AArch64 identity function: a bare `ret`, same payload used
        // elsewhere in this crate's tests. Its content doesn't matter for
        // this test -- only that write()'s protect/write/reprotect
        // sequence genuinely runs on this thread.
        buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
        buf.make_executable()
            .expect("make_executable should succeed");

        // Attempt the illegal write. If write()'s reprotection genuinely
        // ran, this line never returns -- the process receives a fault
        // signal here.
        // SAFETY: deliberately violating memory safety to prove the OS
        // stops us -- `ptr` points at a real page from this process's own
        // MAP_JIT mapping, now nominally executable-not-writable; this is
        // the entire point of the test.
        let ptr = buf.as_ptr() as *mut u8;
        unsafe {
            std::ptr::write_volatile(ptr, 0xFFu8);
        }
        // If we reach here, W^X was NOT enforced -- exit with a distinct,
        // non-zero code so the parent can tell "wrote successfully" apart
        // from "was killed by a signal."
        std::process::exit(123);
    }

    // Parent: wait for the child and confirm it died from a signal.
    let mut status: libc::c_int = 0;
    // SAFETY: pid is the value fork() just returned to us in the parent
    // branch (guaranteed > 0 here since we're not in the pid==0 branch);
    // `status` is a valid, exclusively-owned local we're passing by
    // pointer for waitpid to write into.
    let wait_result = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(
        wait_result,
        pid,
        "waitpid failed: {}",
        std::io::Error::last_os_error()
    );

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
