// crates/forge-mem/tests/wx_enforcement.rs

use forge_mem::ExecutableBuffer;

/// Proves W^X is genuinely enforced by the OS, not just "we didn't call
/// mprotect(W) after make_executable and got lucky." Forks a child process,
/// has the child attempt an illegal write to an executable page, and
/// confirms (in the parent, via waitpid) that the child died from a signal.
/// Running the illegal write in a forked child (not the test process
/// itself) means a genuine W^X violation only kills the disposable child,
/// not the whole test binary.
#[test]
fn writing_to_an_executable_page_segfaults() {
    let mut buf = ExecutableBuffer::new(64).expect("allocation should succeed");
    buf.write(|mem| mem[..4].copy_from_slice(&[0xC0, 0x03, 0x5F, 0xD6]));
    buf.make_executable()
        .expect("make_executable should succeed");
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
        // this line never returns -- the process receives a fault signal
        // here.
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
